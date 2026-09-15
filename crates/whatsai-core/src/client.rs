use crate::{crypto::*, governance::*, protocol::*, service::field, storage};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub struct Client {
    pub dir: PathBuf,
    pub identity: Identity,
    pub db: Connection,
    http: reqwest::Client,
    pub endpoint: Option<iroh::Endpoint>,
}
impl Client {
    pub fn open(dir: &Path, name: &str) -> Result<Self> {
        let identity = storage::identity(dir, name)?;
        let db = storage::database(&dir.join("client.db"), storage::CLIENT_SCHEMA)?;
        db.execute(
            "UPDATE inbox SET dispatch='interrupted' WHERE dispatch='running'",
            [],
        )?;
        Ok(Self {
            dir: dir.into(),
            identity,
            db,
            endpoint: None,
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }
    pub fn config(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row("SELECT value FROM config WHERE key=?", [key], |r| r.get(0))
            .optional()?)
    }
    pub fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.execute(
            "INSERT INTO config VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
    pub fn team(&self) -> Result<Team> {
        serde_json::from_str(&self.config("team")?.context("not a member of a team yet")?)
            .map_err(Into::into)
    }
    pub async fn rpc_at(&self, service: &str, operation: Value) -> Result<Value> {
        validate_service(service)?;
        let request = self.identity.sign(Request {
            version: VERSION,
            nonce: id(),
            timestamp: now(),
            operation,
        })?;
        let response = self
            .http
            .post(format!("{}/v1/rpc", service.trim_end_matches('/')))
            .json(&request)
            .send()
            .await
            .context("authority unreachable; operation remains local")?;
        let body: Value = response.json().await?;
        ensure!(
            body["ok"] == true,
            "{}",
            body["error"].as_str().unwrap_or("invalid service response")
        );
        Ok(body["result"].clone())
    }
    pub async fn rpc(&self, mut operation: Value) -> Result<Value> {
        let team = self.team()?;
        operation["team"] = json!(team.id);
        self.rpc_at(
            &self.config("service")?.context("no service configured")?,
            operation,
        )
        .await
    }
    fn save_team(&self, t: &Team, founder: &str) -> Result<()> {
        verify_team(t, founder)?;
        if let Ok(old) = self.team() {
            ensure!(old.id == t.id, "already bound to a different team");
            ensure!(
                t.history.len() >= old.history.len(),
                "membership rollback detected"
            );
            ensure!(
                serde_json::to_vec(&t.history[..old.history.len()])?
                    == serde_json::to_vec(&old.history)?,
                "membership fork detected"
            );
        }
        self.set("team", &serde_json::to_string(t)?)
    }
    pub async fn refresh(&self) -> Result<Team> {
        let old = self.team()?;
        let mut op = json!({"method":"team"});
        if let Some(ep) = self.config("endpoint")? {
            op["endpoint"] = serde_json::from_str(&ep)?;
        }
        let next: Team = serde_json::from_value(self.rpc(op).await?)?;
        self.save_team(&next, &old.founder)?;
        Ok(next)
    }
    pub async fn create(&self, service: &str, repository: &str) -> Result<Value> {
        ensure!(
            self.config("team")?.is_none() && self.config("joining")?.is_none(),
            "already in a team or joining one"
        );
        validate_remote(repository)?;
        validate_service(service)?;
        let member = self.identity.member()?;
        let record: Signed<Governance> = if let Some(saved) = self.config("creation_intent")? {
            let record: Signed<Governance> = serde_json::from_str(&saved)?;
            ensure!(
                record.body.repository.as_deref() == Some(repository)
                    && self.config("creation_service")?.as_deref() == Some(service),
                "unfinished creation targets a different service/repository"
            );
            record
        } else {
            let record = self.identity.sign(Governance {
                team: id(),
                revision: 0,
                previous: String::new(),
                action: "create".into(),
                member: Some(member.clone()),
                target: None,
                repository: Some(repository.into()),
            })?;
            self.set("creation_service", service)?;
            self.set("creation_intent", &serde_json::to_string(&record)?)?;
            record
        };
        let t: Team = serde_json::from_value(
            self.rpc_at(service, json!({"method":"create","record":record}))
                .await?,
        )?;
        self.set("service", service)?;
        self.save_team(&t, &member.id)?;
        self.invite()
    }
    pub fn invite(&self) -> Result<Value> {
        let t = self.team()?;
        let card = Invite {
            version: VERSION,
            service: self.config("service")?.context("missing service")?,
            team: t.id.clone(),
            founder: t.founder,
        };
        Ok(
            json!({"team":t.id,"join":"whatsai1.".to_owned()+&B64.encode(serde_json::to_vec(&card)?),"notice":"This descriptor requests admission; an admin must approve your fingerprint."}),
        )
    }
    pub async fn join(&self, card: &str) -> Result<Value> {
        ensure!(self.config("team")?.is_none(), "already a team member");
        let card: Invite = serde_json::from_slice(
            &B64.decode(
                card.strip_prefix("whatsai1.")
                    .context("invalid join descriptor")?,
            )?,
        )?;
        ensure!(card.version == VERSION, "unsupported join descriptor");
        validate_service(&card.service)?;
        valid_id(&card.team)?;
        if let Some(existing) = self.config("joining")? {
            ensure!(
                existing == serde_json::to_string(&card)?,
                "already requesting a different team"
            );
        }
        self.set("joining", &serde_json::to_string(&card)?)?;
        self.rpc_at(
            &card.service,
            json!({"method":"request_join","team":card.team,"member":self.identity.member()?}),
        )
        .await
    }
    pub async fn join_status(&self) -> Result<Value> {
        let Some(card) = self.config("joining")? else {
            return Ok(json!({"state":"not-joining"}));
        };
        let card: Invite = serde_json::from_str(&card)?;
        let result = self
            .rpc_at(
                &card.service,
                json!({"method":"join_status","team":card.team}),
            )
            .await?;
        if result["state"] == "admitted" {
            let t: Team = serde_json::from_value(result["team"].clone())?;
            ensure!(t.id == card.team, "team mismatch");
            self.save_team(&t, &card.founder)?;
            self.set("service", &card.service)?;
            self.db
                .execute("DELETE FROM config WHERE key='joining'", [])?;
        }
        Ok(result)
    }
    pub async fn govern(&self, action: &str, target: &str) -> Result<Value> {
        let t = self.refresh().await?;
        let mut member = None;
        if action == "admit" {
            let requests = self.rpc(json!({"method":"requests"})).await?;
            let r = requests
                .as_array()
                .context("invalid requests")?
                .iter()
                .find(|r| r["member"]["id"] == target && r["state"] == "pending")
                .context("no pending request for this fingerprint")?;
            member = Some(serde_json::from_value(r["member"].clone())?);
        }
        let record = self.identity.sign(Governance {
            team: t.id.clone(),
            revision: t.history.len(),
            previous: digest(&serde_json::to_vec(
                t.history.last().context("empty history")?,
            )?),
            action: action.into(),
            member,
            target: if action == "admit" {
                None
            } else {
                Some(target.into())
            },
            repository: None,
        })?;
        let mut log = t.history.clone();
        log.push(record.clone());
        replay(&log, &t.founder)?;
        let next: Team =
            serde_json::from_value(self.rpc(json!({"method":"govern","record":record})).await?)?;
        self.save_team(&next, &t.founder)?;
        Ok(json!(next))
    }
    pub fn enqueue(
        &self,
        kind: &str,
        actor: &str,
        text: &str,
        to: Option<String>,
        reply_to: Option<String>,
        data: Value,
    ) -> Result<String> {
        let t = self.team()?;
        let me = self.identity.member()?.id;
        ensure!(t.members.contains_key(&me), "not a current member");
        if let Some(target) = &to {
            ensure!(t.members.contains_key(target), "unknown recipient");
        }
        let eid = id();
        let root = if let Some(reply) = &reply_to {
            let value: String = self
                .db
                .query_row("SELECT event FROM inbox WHERE id=?", [reply], |r| r.get(0))
                .context("reply target is not in local inbox")?;
            serde_json::from_str::<Event>(&value)?.root
        } else {
            eid.clone()
        };
        let event = Event {
            actor: actor.into(),
            text: text.into(),
            to,
            reply_to,
            root,
            data,
        };
        event.validate()?;
        let members: Vec<_> = t.members.values().cloned().collect();
        let header = Header {
            version: VERSION,
            id: eid.clone(),
            team: t.id,
            sender: me,
            created: now(),
            kind: kind.into(),
            recipients: members.iter().map(|m| m.id.clone()).collect(),
        };
        let envelope = self
            .identity
            .seal(header, &serde_json::to_vec(&event)?, &members)?;
        self.db.execute(
            "INSERT INTO outbox(id,envelope) VALUES(?,?)",
            params![eid, serde_json::to_string(&envelope)?],
        )?;
        Ok(eid)
    }
    pub async fn sync(&mut self) -> Result<Value> {
        if self.config("joining")?.is_some() {
            let r = self.join_status().await?;
            if r["state"] != "admitted" {
                return Ok(r);
            }
        }
        if self.config("team")?.is_none() {
            return Ok(json!({"state":"unbound"}));
        }
        let team = self.refresh().await?;
        let pending: Vec<(String, String)> = self
            .db
            .prepare("SELECT id,envelope FROM outbox WHERE state='queued' ORDER BY rowid")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (eid, body) in pending {
            let envelope: Signed<Sealed> = serde_json::from_str(&body)?;
            // Never silently re-encrypt a queued event for a changed roster.
            if envelope.body.header.recipients != team.members.keys().cloned().collect::<Vec<_>>() {
                self.db.execute("UPDATE outbox SET state='failed',error='Membership changed; explicitly resend with the current roster' WHERE id=?",[&eid])?;
                continue;
            }
            match self.rpc(json!({"method":"put","envelope":envelope})).await {
                Ok(_) => {
                    self.db.execute(
                        "UPDATE outbox SET state='service-stored',error=NULL WHERE id=?",
                        [&eid],
                    )?;
                }
                Err(e) => {
                    self.db.execute(
                        "UPDATE outbox SET error=? WHERE id=?",
                        params![e.to_string(), eid],
                    )?;
                }
            }
        }
        let chunks:Vec<(String,i64,String)>=self.db.prepare("SELECT c.file,c.idx,c.envelope FROM chunks c JOIN outbox o ON o.id=c.file WHERE c.state='queued' AND o.state='service-stored'")?.query_map([],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (file, index, body) in chunks {
            let env: Value = serde_json::from_str(&body)?;
            self.rpc(json!({"method":"put_chunk","file":file,"index":index,"envelope":env}))
                .await?;
            self.db.execute(
                "UPDATE chunks SET state='service-stored' WHERE file=? AND idx=?",
                params![file, index],
            )?;
        }
        let cursor = self
            .config("cursor")?
            .unwrap_or_else(|| "0".into())
            .parse::<i64>()?;
        let events = self.rpc(json!({"method":"pull","cursor":cursor})).await?;
        let mut received = 0;
        for row in events.as_array().context("invalid mailbox")? {
            let env: Signed<Sealed> = serde_json::from_value(row["envelope"].clone())?;
            let seq = row["seq"].as_i64().context("missing sequence")?;
            self.receive(seq, &env)?;
            self.rpc(json!({"method":"ack","event":env.body.header.id}))
                .await?;
            self.set("cursor", &seq.to_string())?;
            received += 1;
        }
        Ok(json!({"state":"synced","received":received}))
    }
    pub fn receive(&mut self, seq: i64, env: &Signed<Sealed>) -> Result<()> {
        let t = self.team()?;
        ensure!(env.body.header.team == t.id, "wrong team");
        let event: Event = serde_json::from_slice(&self.identity.open(env)?)?;
        event.validate()?;
        // The authority only returns originally authorized recipients; the sender may since have left.
        ensure!(
            t.history
                .iter()
                .any(|g| g.body.member.as_ref().is_some_and(|m| m.id == env.signer)),
            "unknown historical sender"
        );
        let tx = self.db.transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO inbox(id,seq,event,envelope) VALUES(?,?,?,?)",
            params![
                env.body.header.id,
                seq,
                serde_json::to_string(&event)?,
                serde_json::to_string(env)?
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn inbox(&self) -> Result<Value> {
        let mut q = self
            .db
            .prepare("SELECT id,seq,event,envelope,dispatch FROM inbox ORDER BY seq")?;
        let rows = q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (id, seq, event, env, dispatch) = row?;
            let env: Signed<Sealed> = serde_json::from_str(&env)?;
            out.push(json!({"id":id,"sequence":seq,"sender":env.signer,"kind":env.body.header.kind,"created":env.body.header.created,"event":serde_json::from_str::<Value>(&event)?,"dispatch":dispatch}));
        }
        Ok(json!(out))
    }
    pub async fn outbox(&self) -> Result<Value> {
        let rows: Vec<(String, String, String, Option<String>)> = self
            .db
            .prepare("SELECT id,state,envelope,error FROM outbox ORDER BY rowid")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = vec![];
        for (eid, state, body, error) in rows {
            let env: Signed<Sealed> = serde_json::from_str(&body)?;
            let mut item =
                json!({"id":eid,"state":state,"error":error,"kind":env.body.header.kind});
            if state == "service-stored"
                && let Ok(receipts) = self.rpc(json!({"method":"receipts","event":eid})).await
            {
                if receipts["expired"] == true {
                    item["state"] = json!("expired");
                }
                item["receipts"] = receipts;
            }
            if env.body.header.kind == "file" {
                let queued: i64 = self.db.query_row(
                    "SELECT count(*) FROM chunks WHERE file=? AND state!='service-stored'",
                    [&eid],
                    |r| r.get(0),
                )?;
                if queued > 0 && state == "service-stored" {
                    item["state"] = json!("uploading");
                }
                item["chunks_pending"] = json!(queued);
            }
            out.push(item);
        }
        Ok(json!(out))
    }
    pub fn share(&mut self, path: &Path, actor: &str) -> Result<String> {
        let mut f = std::fs::File::open(path)?;
        let meta = f.metadata()?;
        ensure!(
            meta.is_file() && meta.len() <= MAX_FILE as u64,
            "file must be regular and at most 32 MiB"
        );
        use std::io::Read;
        let mut content = vec![];
        std::io::Read::by_ref(&mut f)
            .take((MAX_FILE + 1) as u64)
            .read_to_end(&mut content)?;
        ensure!(content.len() <= MAX_FILE, "file grew beyond limit");
        let manifest = Manifest {
            name: path
                .file_name()
                .and_then(|x| x.to_str())
                .context("invalid filename")?
                .into(),
            size: content.len(),
            sha256: digest(&content),
            chunks: content.chunks(CHUNK_SIZE).map(digest).collect(),
        };
        manifest.validate()?;
        // Transaction encloses manifest and chunks: a crash cannot expose a manifest without its local bytes.
        self.db.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> Result<String> {
            let eid = self.enqueue(
                "file",
                actor,
                "Shared a file",
                None,
                None,
                serde_json::to_value(&manifest)?,
            )?;
            let body: String =
                self.db
                    .query_row("SELECT envelope FROM outbox WHERE id=?", [&eid], |r| {
                        r.get(0)
                    })?;
            let base: Signed<Sealed> = serde_json::from_str(&body)?;
            let members: Vec<_> = self.team()?.members.values().cloned().collect();
            for (index, chunk) in content.chunks(CHUNK_SIZE).enumerate() {
                let mut h = base.body.header.clone();
                h.kind = format!("chunk/{index}");
                let env = self.identity.seal(h, chunk, &members)?;
                self.db.execute(
                    "INSERT INTO chunks(file,idx,envelope) VALUES(?,?,?)",
                    params![eid, index as i64, serde_json::to_string(&env)?],
                )?;
            }
            Ok(eid)
        })();
        match result {
            Ok(eid) => {
                self.db.execute_batch("COMMIT")?;
                Ok(eid)
            }
            Err(e) => {
                self.db.execute_batch("ROLLBACK")?;
                Err(e)
            }
        }
    }
    pub async fn download(
        &self,
        file: &str,
        directory: &Path,
        max_chunks: Option<usize>,
    ) -> Result<Value> {
        valid_id(file)?;
        let body: String = self
            .db
            .query_row("SELECT event FROM inbox WHERE id=?", [file], |r| r.get(0))
            .context("file manifest is not in inbox; sync first")?;
        let event: Event = serde_json::from_str(&body)?;
        let manifest: Manifest = serde_json::from_value(event.data)?;
        manifest.validate()?;
        let env_str: String =
            self.db
                .query_row("SELECT envelope FROM inbox WHERE id=?", [file], |r| {
                    r.get(0)
                })?;
        let env: Signed<Sealed> = serde_json::from_str(&env_str)?;
        ensure!(env.body.header.kind == "file", "not a file");
        let mut fetched = 0;
        for (i, expected) in manifest.chunks.iter().enumerate() {
            let existing: Option<Vec<u8>> = self
                .db
                .query_row(
                    "SELECT bytes FROM downloads WHERE file=? AND idx=?",
                    params![file, i as i64],
                    |r| r.get(0),
                )
                .optional()?;
            if existing.as_ref().is_some_and(|b| digest(b) == *expected) {
                continue;
            }
            if max_chunks.is_some_and(|limit| fetched >= limit) {
                return Ok(json!({"state":"partial","file":file,"new_chunks":fetched}));
            }
            let mut peer_chunk = None;
            if let Some(endpoint) = &self.endpoint {
                let team = self.team()?;
                if let Some(address) = team.endpoints.get(&env.signer) {
                    let req=self.identity.sign(Request{version:VERSION,nonce:id(),timestamp:now(),operation:json!({"method":"fetch_chunk","file":file,"index":i,"team":team.id})})?;
                    if let Ok((reply, path)) = crate::transport::exchange(
                        endpoint,
                        serde_json::from_value(address.clone())?,
                        &json!({"method":"fetch_chunk","request":req}),
                    )
                    .await
                        && reply["ok"] == true
                    {
                        peer_chunk = Some(reply["envelope"].clone());
                        self.set("last_file_path", &path)?;
                    }
                }
            }
            let chunk: Signed<Sealed> = serde_json::from_value(match peer_chunk {
                Some(v) => v,
                None => {
                    self.set("last_file_path", "mailbox")?;
                    self.rpc(json!({"method":"get_chunk","file":file,"index":i}))
                        .await?
                }
            })?;
            ensure!(
                chunk.signer == env.signer
                    && chunk.body.header.id == file
                    && chunk.body.header.team == env.body.header.team
                    && chunk.body.header.kind == format!("chunk/{i}"),
                "chunk context mismatch"
            );
            let bytes = self.identity.open(&chunk)?;
            ensure!(
                bytes.len() <= CHUNK_SIZE && digest(&bytes) == *expected,
                "chunk integrity mismatch"
            );
            self.db.execute("INSERT INTO downloads VALUES(?,?,?) ON CONFLICT(file,idx) DO UPDATE SET bytes=excluded.bytes",params![file,i as i64,bytes])?;
            fetched += 1;
        }
        let mut bytes = vec![];
        for i in 0..manifest.chunks.len() {
            let chunk: Vec<u8> = self.db.query_row(
                "SELECT bytes FROM downloads WHERE file=? AND idx=?",
                params![file, i as i64],
                |r| r.get(0),
            )?;
            bytes.extend(chunk);
        }
        ensure!(
            bytes.len() == manifest.size && digest(&bytes) == manifest.sha256,
            "whole file integrity mismatch"
        );
        ensure!(directory.is_dir(), "download directory does not exist");
        let dest = directory.join(&manifest.name);
        // Hard-link publication is atomic and fails if destination exists, including a symlink.
        let temp = directory.join(format!(".whatsai-{}.part", id()));
        storage::write_private(&temp, &bytes)?;
        let published = std::fs::hard_link(&temp, &dest);
        let _ = std::fs::remove_file(&temp);
        published.context("destination exists or cannot be published; choose another directory")?;
        std::fs::File::open(directory)?.sync_all()?;
        Ok(
            json!({"state":"complete","path":dest,"sha256":manifest.sha256,"bytes":manifest.size,"new_chunks":fetched}),
        )
    }
    pub fn statuses(&self) -> Result<Value> {
        let inbox = self.inbox()?;
        let mut statuses = BTreeMap::new();
        for v in inbox.as_array().context("invalid inbox")? {
            if v["kind"] == "status" {
                statuses.insert(
                    v["sender"].as_str().unwrap_or_default().to_owned(),
                    v.clone(),
                );
            }
        }
        let t = self.team()?;
        let mut out = vec![];
        for (member, status) in statuses {
            out.push(json!({"member":member,"status":status,"connected":t.presence.get(&member).is_some_and(|seen|now()-seen<15)}));
        }
        Ok(json!(out))
    }
    pub async fn command(&mut self, cmd: Value) -> Result<Value> {
        let action = field(&cmd, "action")?;
        match action {
            "worker" => self.worker_command(&cmd),
            "accept-handoff" => {
                self.accept_handoff(
                    field(&cmd, "event")?,
                    Path::new(field(&cmd, "repo")?),
                    Path::new(field(&cmd, "directory")?),
                )
                .await
            }
            "files" => Ok(json!(
                self.inbox()?
                    .as_array()
                    .context("invalid inbox")?
                    .iter()
                    .filter(|v| v["kind"] == "file")
                    .collect::<Vec<_>>()
            )),
            "health" => Ok(
                json!({"version":VERSION,"member":self.identity.member()?,"team":self.config("team")?.map(|t|serde_json::from_str::<Value>(&t)).transpose()?,"last_sync_error":self.config("last_sync_error")?,"last_peer_path":self.config("last_peer_path")?,"last_file_path":self.config("last_file_path")?}),
            ),
            "register" => Ok(json!(self.identity.member()?)),
            "create" => {
                self.create(field(&cmd, "service")?, field(&cmd, "repository")?)
                    .await
            }
            "invite" => self.invite(),
            "join" => self.join(field(&cmd, "descriptor")?).await,
            "join-status" => self.join_status().await,
            "list" => Ok(json!(self.refresh().await?)),
            "requests" => self.rpc(json!({"method":"requests"})).await,
            "approve" => self.govern("admit", field(&cmd, "member")?).await,
            "reject" => {
                self.rpc(json!({"method":"reject","member":field(&cmd,"member")?}))
                    .await
            }
            "promote" | "demote" | "revoke" => self.govern(action, field(&cmd, "member")?).await,
            "leave" => self.govern("leave", &self.identity.member()?.id).await,
            "sync" => self.sync().await,
            "inbox" => self.inbox(),
            "outbox" => self.outbox().await,
            "send" | "agent-send" => {
                let eid = self.enqueue(
                    "message",
                    if action == "send" { "person" } else { "agent" },
                    field(&cmd, "text")?,
                    cmd["to"].as_str().map(String::from),
                    cmd["reply_to"].as_str().map(String::from),
                    Value::Null,
                )?;
                Ok(json!({"id":eid,"state":"queued"}))
            }
            "share" => Ok(
                json!({"id":self.share(Path::new(field(&cmd,"path")?),cmd["actor"].as_str().unwrap_or("person"))?,"state":"queued"}),
            ),
            "download" => {
                self.download(
                    field(&cmd, "file")?,
                    Path::new(field(&cmd, "directory")?),
                    cmd["max_chunks"].as_u64().map(|n| n as usize),
                )
                .await
            }
            "status" => {
                if let Some(state) = cmd["state"].as_str() {
                    ensure!(
                        ["working", "blocked", "ready"].contains(&state),
                        "invalid work status"
                    );
                    let eid = self.enqueue(
                        "status",
                        cmd["actor"].as_str().unwrap_or("person"),
                        cmd["description"].as_str().unwrap_or(""),
                        None,
                        None,
                        json!({"state":state,"branch":cmd["branch"],"commit":cmd["commit"]}),
                    )?;
                    Ok(json!({"id":eid,"state":"queued"}))
                } else {
                    self.statuses()
                }
            }
            "handoff" => {
                let commit = field(&cmd, "commit")?;
                ensure!(
                    [40, 64].contains(&commit.len()) && hex::decode(commit).is_ok(),
                    "expected full commit hash"
                );
                let eid=self.enqueue("handoff",cmd["actor"].as_str().unwrap_or("person"),cmd["description"].as_str().unwrap_or(""),None,None,json!({"repository":self.team()?.id,"commit":commit,"branch":field(&cmd,"branch")?}))?;
                Ok(json!({"id":eid,"state":"queued"}))
            }
            _ => bail!("unknown local operation"),
        }
    }
}

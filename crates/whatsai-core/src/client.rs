//! The local member: one identity, any number of teams, each bound to a workspace.
use crate::{
    crypto::*,
    governance::*,
    protocol::*,
    service::{Service, field},
    storage,
};
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
use iroh::EndpointAddr;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// id, state, envelope, error, team.
type OutboxRow = (String, String, String, Option<String>, Option<String>);
pub struct Client {
    pub dir: PathBuf,
    pub identity: Identity,
    pub db: Connection,
    pub endpoint: Option<iroh::Endpoint>,
    /// The authority this daemon can host for teams it founded; served to peers over iroh.
    pub authority: Option<Arc<Service>>,
}
/// One team this member belongs to, as kept locally.
#[derive(Clone, Debug)]
pub struct Membership {
    pub id: String,
    pub founder: String,
    pub authority: EndpointAddr,
    pub secret: String,
    pub team: Team,
    pub cursor: i64,
    /// The local directory this team was created from or joined for, when path-bound.
    pub path: Option<String>,
}
impl Client {
    pub fn open(dir: &Path, name: &str) -> Result<Self> {
        let identity = storage::identity(dir, name)?;
        let db = storage::database(&dir.join("client.db"), storage::CLIENT_MIGRATIONS)?;
        db.execute(
            "UPDATE inbox SET dispatch='interrupted' WHERE dispatch='running'",
            [],
        )?;
        Ok(Self {
            dir: dir.into(),
            identity,
            db,
            endpoint: None,
            authority: None,
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

    // ------------------------------------------------------------------ teams
    pub fn memberships(&self) -> Result<Vec<Membership>> {
        let mut q = self.db.prepare(
            "SELECT id,founder,authority,secret,team,cursor,path FROM teams ORDER BY joined,id",
        )?;
        let rows = q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, i64>(5)?,
                r.get::<_, Option<String>>(6)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (id, founder, authority, secret, team, cursor, path) = row?;
            out.push(Membership {
                id,
                founder,
                authority: serde_json::from_str(&authority)?,
                secret,
                team: serde_json::from_str(&team)?,
                cursor,
                path,
            });
        }
        Ok(out)
    }
    pub fn membership(&self, id: &str) -> Result<Membership> {
        self.memberships()?
            .into_iter()
            .find(|m| m.id == id)
            .with_context(|| format!("not a member of team {id}"))
    }
    pub fn team(&self, id: &str) -> Result<Team> {
        Ok(self.membership(id)?.team)
    }
    /// Record membership of a team locally (creation, admission, or a test fixture).
    pub fn install_team(
        &self,
        team: &Team,
        authority: &EndpointAddr,
        secret: &str,
        path: Option<&Path>,
    ) -> Result<()> {
        verify_team(team, &team.founder)?;
        self.db.execute(
            "INSERT INTO teams(id,founder,authority,secret,team,cursor,path,joined) VALUES(?,?,?,?,?,0,?,?) ON CONFLICT(id) DO UPDATE SET team=excluded.team,authority=excluded.authority,secret=excluded.secret,path=COALESCE(excluded.path,teams.path)",
            params![team.id, team.founder, serde_json::to_string(authority)?, secret, serde_json::to_string(team)?, path.map(|p| p.to_string_lossy().into_owned()), now()],
        )?;
        Ok(())
    }
    fn save_team(&self, t: &Team) -> Result<()> {
        let old = self.membership(&t.id)?;
        verify_team(t, &old.founder)?;
        ensure!(
            t.history.len() >= old.team.history.len(),
            "membership rollback detected"
        );
        ensure!(
            serde_json::to_vec(&t.history[..old.team.history.len()])?
                == serde_json::to_vec(&old.team.history)?,
            "membership fork detected"
        );
        self.db.execute(
            "UPDATE teams SET team=? WHERE id=?",
            params![serde_json::to_string(t)?, t.id],
        )?;
        Ok(())
    }
    /// Teams as the owner sees them, with pending joins and unfinished creations.
    pub fn teams(&self) -> Result<Value> {
        let me = self.identity.member()?.id;
        let mut out = vec![];
        for m in self.memberships()? {
            out.push(json!({
                "id":m.id,
                "workspace":m.team.workspace,
                "repository":m.team.repository,
                "founder":m.founder,
                "role":if m.team.admins.contains(&me){"admin"}else{"member"},
                "members":m.team.members.len(),
                "path":m.path,
                "state":"member",
            }));
        }
        let mut q = self
            .db
            .prepare("SELECT id,invite,path,requested FROM joining ORDER BY requested")?;
        let rows = q.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?;
        for row in rows {
            let (id, invite, path, requested) = row?;
            let invite: Invite = serde_json::from_str(&invite)?;
            out.push(json!({"id":id,"workspace":invite.workspace,"founder":invite.founder,"path":path,"state":"joining","requested":requested}));
        }
        Ok(json!(out))
    }
    /// A team named by id, workspace name, or repository.
    pub fn find_team(&self, selector: &str) -> Result<String> {
        let selector = selector.trim();
        let matches: Vec<Membership> = self
            .memberships()?
            .into_iter()
            .filter(|m| {
                m.id == selector
                    || m.team.workspace == selector
                    || m.team.repository.as_deref() == Some(selector)
            })
            .collect();
        match matches.len() {
            1 => Ok(matches[0].id.clone()),
            0 => bail!("no team matches {selector:?}; run teams to see them"),
            _ => bail!("{selector:?} matches several teams; use the team id"),
        }
    }
    /// The team a directory belongs to: by its Git origin for repository-bound teams, by exact
    /// path for path-bound ones. Never guesses from a directory name alone.
    pub fn match_workspace(&self, path: &Path) -> Result<Option<String>> {
        let canonical = std::fs::canonicalize(path)?;
        let origin = crate::agents::detect_repository(&canonical);
        let canonical = canonical.to_string_lossy().into_owned();
        for m in self.memberships()? {
            match (&m.team.repository, &origin) {
                (Some(repo), Some(found)) if repo == found => return Ok(Some(m.id)),
                (None, _) if m.path.as_deref() == Some(canonical.as_str()) => {
                    return Ok(Some(m.id));
                }
                _ => {}
            }
        }
        Ok(None)
    }
    /// Which team a command means: explicit selector, the session's agent, the working
    /// directory, or the only team there is.
    pub fn resolve_team(&self, cmd: &Value) -> Result<String> {
        if let Some(selector) = cmd["team"].as_str() {
            return self.find_team(selector);
        }
        if let Some(via) = cmd["via"].as_str()
            && let Some(team) = self.is_enrolled(via)?
        {
            return Ok(team);
        }
        if let Some(cwd) = cmd["cwd"].as_str()
            && let Some(team) = self.match_workspace(Path::new(cwd))?
        {
            return Ok(team);
        }
        let all = self.memberships()?;
        match all.len() {
            1 => Ok(all[0].id.clone()),
            0 => bail!("not a member of a team yet"),
            _ => bail!(
                "this directory is not bound to one of your {} teams; pass --team WORKSPACE",
                all.len()
            ),
        }
    }

    // ------------------------------------------------------------------- rpc
    /// Send one signed authority operation to the daemon at `authority`, or answer it locally when
    /// this daemon is that authority.
    pub async fn rpc_at(&self, authority: &EndpointAddr, operation: Value) -> Result<Value> {
        let request = self.identity.sign(Request {
            version: VERSION,
            nonce: id(),
            timestamp: now(),
            operation,
        })?;
        let hosted_here = self
            .endpoint
            .as_ref()
            .is_none_or(|endpoint| endpoint.id() == authority.id);
        if hosted_here && let Some(service) = &self.authority {
            let service = service.clone();
            return tokio::task::spawn_blocking(move || service.handle(request)).await?;
        }
        let endpoint = self
            .endpoint
            .as_ref()
            .context("daemon networking unavailable")?;
        let (reply, _) = crate::transport::exchange_within(
            endpoint,
            authority.clone(),
            &json!({"method":"rpc","request":request}),
            Duration::from_secs(45),
        )
        .await
        .context("authority unreachable; operation remains local")?;
        ensure!(
            reply["ok"] == true,
            "{}",
            reply["error"]
                .as_str()
                .unwrap_or("invalid authority response")
        );
        Ok(reply["result"].clone())
    }
    pub async fn rpc(&self, team: &str, mut operation: Value) -> Result<Value> {
        let m = self.membership(team)?;
        operation["team"] = json!(m.id);
        self.rpc_at(&m.authority, operation).await
    }
    /// Wait briefly for a relay connection so a freshly published address reaches across NATs.
    pub async fn wait_online(&self, limit: Duration) {
        if let Some(endpoint) = &self.endpoint
            && endpoint.addr().relay_urls().next().is_none()
        {
            let _ = tokio::time::timeout(limit, endpoint.online()).await;
        }
    }
    /// This daemon's address; without a bound endpoint (tests, offline tools) it is the bare
    /// node id, which is still what a local authority is keyed by.
    fn own_address(&self) -> Result<EndpointAddr> {
        if let Some(endpoint) = &self.endpoint {
            return Ok(endpoint.addr());
        }
        let secret: [u8; 32] = hex::decode(&self.identity.transport)?
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid transport key"))?;
        Ok(EndpointAddr::new(
            iroh::SecretKey::from_bytes(&secret).public(),
        ))
    }
    pub async fn refresh(&self, team: &str) -> Result<Team> {
        let mut op = json!({"method":"team","agents":self.agent_presence(team)?});
        if let Some(ep) = self.config("endpoint")? {
            op["endpoint"] = serde_json::from_str(&ep)?;
        }
        let next: Team = serde_json::from_value(self.rpc(team, op).await?)?;
        self.save_team(&next)?;
        // Follow the founder's current address so relay or IP changes do not strand the team.
        if let Some(address) = next.endpoints.get(&next.founder) {
            self.db.execute(
                "UPDATE teams SET authority=? WHERE id=?",
                params![serde_json::to_string(address)?, team],
            )?;
        }
        Ok(next)
    }

    // ------------------------------------------------------- create and join
    /// Create a team bound to `workspace`, and to `repository` when there is one: this daemon
    /// becomes the network's authority and mints the network secret.
    pub async fn create(&self, repository: Option<&str>, workspace: &Path) -> Result<Value> {
        let path = std::fs::canonicalize(workspace).context("workspace does not exist")?;
        ensure!(path.is_dir(), "workspace is not a directory");
        let repository = repository.map(str::trim).filter(|r| !r.is_empty());
        if let Some(remote) = repository {
            validate_remote(remote)?;
        }
        let name = workspace_name(&path);
        validate_workspace_name(&name)?;
        ensure!(
            self.match_workspace(&path)?.is_none(),
            "this workspace already belongs to one of your teams"
        );
        ensure!(
            self.authority.is_some(),
            "this daemon cannot host a team authority"
        );
        let member = self.identity.member()?;
        let path_text = path.to_string_lossy().into_owned();
        let saved: Option<(String, String, String, Option<String>)> = self
            .db
            .query_row(
                "SELECT id,record,secret,repository FROM creating WHERE path=?",
                [&path_text],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let (record, secret): (Signed<Governance>, String) = match saved {
            Some((_, record, secret, saved_repository)) => {
                ensure!(
                    saved_repository.as_deref() == repository,
                    "unfinished creation for this workspace targets a different repository"
                );
                (serde_json::from_str(&record)?, secret)
            }
            None => {
                let record = self.identity.sign(Governance {
                    team: id(),
                    revision: 0,
                    previous: String::new(),
                    action: "create".into(),
                    member: Some(member.clone()),
                    target: None,
                    repository: repository.map(str::to_owned),
                    workspace: Some(name.clone()),
                })?;
                let mut bytes = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut bytes);
                let secret = hex::encode(bytes);
                self.db.execute(
                    "INSERT INTO creating(id,record,secret,path,workspace,repository) VALUES(?,?,?,?,?,?)",
                    params![record.body.team, serde_json::to_string(&record)?, secret, path_text, name, repository],
                )?;
                (record, secret)
            }
        };
        self.wait_online(Duration::from_secs(5)).await;
        let address = self.own_address()?;
        let t: Team = serde_json::from_value(
            self.rpc_at(
                &address,
                json!({"method":"create","record":record,"secret":secret}),
            )
            .await?,
        )?;
        self.install_team(&t, &address, &secret, Some(&path))?;
        self.db
            .execute("DELETE FROM creating WHERE id=?", [&t.id])?;
        self.invite(&t.id)
    }
    /// The join key. Founders embed their live address so the key improves once a relay is known.
    pub fn invite(&self, team: &str) -> Result<Value> {
        let m = self.membership(team)?;
        let me = self.identity.member()?.id;
        let authority = if me == m.founder {
            self.own_address()?
        } else {
            m.authority.clone()
        };
        let relay = authority.relay_urls().next().is_some();
        // With a relay, the node id and relay URL are enough to be found anywhere; direct
        // addresses are learned through the relay. Without one, the LAN addresses are all there is.
        let authority = if relay {
            EndpointAddr::from_parts(
                authority.id,
                authority
                    .relay_urls()
                    .cloned()
                    .map(iroh::TransportAddr::Relay),
            )
        } else {
            authority
        };
        let card = Invite {
            version: VERSION,
            team: m.id.clone(),
            founder: m.founder.clone(),
            secret: m.secret.clone(),
            authority: serde_json::to_value(&authority)?,
            workspace: m.team.workspace.clone(),
        };
        Ok(json!({
            "team":m.id,
            "workspace":m.team.workspace,
            "repository":m.team.repository,
            "join":"whatsai1.".to_owned()+&B64.encode(serde_json::to_vec(&card)?),
            "relay":relay,
            "notice":if relay {"This key requests admission; an admin must approve your fingerprint."} else {"No relay connection yet: this key only reaches the founder on the local network. Re-run invite once the daemon is online."}
        }))
    }
    /// Ask a network's authority for admission using a join key, for the given local workspace.
    pub async fn join(&self, key: &str, workspace: &Path) -> Result<Value> {
        let card: Invite = serde_json::from_slice(
            &B64.decode(key.strip_prefix("whatsai1.").context("invalid join key")?)?,
        )?;
        ensure!(card.version == VERSION, "unsupported join key");
        valid_id(&card.team)?;
        validate_secret(&card.secret)?;
        ensure!(
            hex::decode(&card.founder)
                .map(|b| b.len() == 32)
                .unwrap_or(false),
            "invalid founder fingerprint"
        );
        // The same identity holding the key, from another checkout: that is an enrolment of this
        // workspace into a team it already belongs to, not an admission.
        if let Ok(m) = self.membership(&card.team) {
            let enrolled = self.enroll_path(&m.id, workspace)?;
            return Ok(json!({
                "state":"enrolled",
                "team":m.id,
                "workspace":m.team.workspace,
                "agents":enrolled,
                "notice":"This identity already belongs to that team; the workspace's agents are now enrolled in it. Publish an agent to make it visible to teammates."
            }));
        }
        let authority: EndpointAddr = serde_json::from_value(card.authority.clone())?;
        let path = std::fs::canonicalize(workspace)
            .ok()
            .map(|p| p.to_string_lossy().into_owned());
        self.db.execute(
            "INSERT INTO joining(id,invite,path,requested) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET invite=excluded.invite,path=COALESCE(excluded.path,joining.path)",
            params![card.team, serde_json::to_string(&card)?, path, now()],
        )?;
        let mut result = self
            .rpc_at(
                &authority,
                json!({"method":"request_join","team":card.team,"member":self.identity.member()?,"secret":card.secret}),
            )
            .await?;
        result["team"] = json!(card.team);
        result["workspace"] = json!(card.workspace);
        if result["state"] == "admitted" {
            self.join_status().await?;
        }
        Ok(result)
    }
    /// Check every pending join; admitted ones become memberships.
    pub async fn join_status(&self) -> Result<Value> {
        let mut q = self
            .db
            .prepare("SELECT id,invite,path FROM joining ORDER BY requested")?;
        let pending: Vec<(String, String, Option<String>)> = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(q);
        let mut out = vec![];
        for (team, invite, path) in pending {
            let card: Invite = serde_json::from_str(&invite)?;
            let authority: EndpointAddr = serde_json::from_value(card.authority.clone())?;
            let mut result = match self
                .rpc_at(&authority, json!({"method":"join_status","team":team}))
                .await
            {
                Ok(v) => v,
                Err(e) => json!({"state":"unknown","error":e.to_string()}),
            };
            if result["state"] == "admitted" {
                let t: Team = serde_json::from_value(result["team"].clone())?;
                ensure!(
                    t.id == card.team && t.founder == card.founder,
                    "team mismatch"
                );
                self.install_team(&t, &authority, &card.secret, path.as_deref().map(Path::new))?;
                self.db.execute("DELETE FROM joining WHERE id=?", [&team])?;
                // A checkout waiting on this admission joins the team's agents right away.
                self.enroll_workspace(&t.id)?;
            }
            result["team"] = json!(team);
            result["workspace"] = json!(card.workspace);
            out.push(result);
        }
        Ok(json!(out))
    }
    pub async fn govern(&self, team: &str, action: &str, target: &str) -> Result<Value> {
        let t = self.refresh(team).await?;
        let mut member = None;
        if action == "admit" {
            let requests = self.rpc(team, json!({"method":"requests"})).await?;
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
            workspace: None,
        })?;
        let mut log = t.history.clone();
        log.push(record.clone());
        replay(&log, &t.founder)?;
        let next: Team = serde_json::from_value(
            self.rpc(team, json!({"method":"govern","record":record}))
                .await?,
        )?;
        self.save_team(&next)?;
        Ok(json!(next))
    }
    /// Leaving drops the local membership and every agent's enrolment in it.
    pub async fn leave(&self, team: &str) -> Result<Value> {
        let me = self.identity.member()?.id;
        let result = self.govern(team, "leave", &me).await?;
        self.db.execute(
            "UPDATE agents SET enrolled=0,published=0,team=NULL WHERE team=?",
            [team],
        )?;
        self.db.execute("DELETE FROM teams WHERE id=?", [team])?;
        Ok(result)
    }

    // ---------------------------------------------------------------- events
    #[allow(clippy::too_many_arguments)]
    pub fn enqueue(
        &self,
        team: &str,
        kind: &str,
        actor: &str,
        text: &str,
        to: Option<String>,
        reply_to: Option<String>,
        data: Value,
        agent: Option<String>,
        to_agent: Option<String>,
    ) -> Result<String> {
        let t = self.team(team)?;
        let me = self.identity.member()?.id;
        ensure!(t.members.contains_key(&me), "not a current member");
        if let Some(target) = &to {
            ensure!(t.members.contains_key(target), "unknown recipient");
        }
        if let Some(label) = &agent {
            let info = self
                .agent(label)
                .context("sending agent is not registered here")?;
            ensure!(info["retired"] != true, "sending agent is retired");
            ensure!(
                info["team"] == team,
                "agent {label} is not enrolled in this team"
            );
        }
        // A fresh address must name an agent the recipient has published; a reply reuses the
        // label that just wrote to us, which is evidence enough even if presence lags.
        if let (Some(label), Some(target), None) = (&to_agent, &to, &reply_to)
            && let Some(published) = t.agents.get(target).and_then(|a| a.as_array())
        {
            ensure!(
                published.iter().any(|a| a["label"] == *label),
                "recipient has not published an agent named {label}; they publish with `whatsai agent publish`, and list shows what is published"
            );
        }
        let eid = id();
        let root = if let Some(reply) = &reply_to {
            let (value, reply_team): (String, Option<String>) = self
                .db
                .query_row("SELECT event,team FROM inbox WHERE id=?", [reply], |r| {
                    Ok((r.get(0)?, r.get(1)?))
                })
                .context("reply target is not in local inbox")?;
            ensure!(
                reply_team.as_deref() == Some(team),
                "reply target belongs to another team"
            );
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
            agent,
            to_agent,
        };
        event.validate()?;
        let members: Vec<_> = t.members.values().cloned().collect();
        let header = Header {
            version: VERSION,
            id: eid.clone(),
            team: t.id.clone(),
            sender: me,
            created: now(),
            kind: kind.into(),
            recipients: members.iter().map(|m| m.id.clone()).collect(),
        };
        let envelope = self
            .identity
            .seal(header, &serde_json::to_vec(&event)?, &members)?;
        self.db.execute(
            "INSERT INTO outbox(id,envelope,team) VALUES(?,?,?)",
            params![eid, serde_json::to_string(&envelope)?, t.id],
        )?;
        Ok(eid)
    }
    /// Sync every pending join and every team; one unreachable authority never blocks the rest.
    pub async fn sync(&mut self) -> Result<Value> {
        let joins = self.join_status().await?;
        let mut teams = serde_json::Map::new();
        let mut errors = vec![];
        for m in self.memberships()? {
            match self.sync_team(&m.id).await {
                Ok(v) => {
                    teams.insert(m.id, v);
                }
                Err(e) => {
                    errors.push(format!("{}: {e:#}", m.team.workspace));
                    teams.insert(m.id, json!({"state":"error","error":e.to_string()}));
                }
            }
        }
        if teams.is_empty() && joins.as_array().is_some_and(|j| j.is_empty()) {
            return Ok(json!({"state":"unbound"}));
        }
        Ok(
            json!({"state":if errors.is_empty(){"synced"}else{"partial"},"teams":teams,"joining":joins,"errors":errors}),
        )
    }
    async fn sync_team(&mut self, team: &str) -> Result<Value> {
        let t = self.refresh(team).await?;
        let pending: Vec<(String, String)> = self
            .db
            .prepare(
                "SELECT id,envelope FROM outbox WHERE state='queued' AND team=? ORDER BY rowid",
            )?
            .query_map([team], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (eid, body) in pending {
            let envelope: Signed<Sealed> = serde_json::from_str(&body)?;
            // Never silently re-encrypt a queued event for a changed roster.
            if envelope.body.header.recipients != t.members.keys().cloned().collect::<Vec<_>>() {
                self.db.execute("UPDATE outbox SET state='failed',error='Membership changed; explicitly resend with the current roster' WHERE id=?",[&eid])?;
                continue;
            }
            match self
                .rpc(team, json!({"method":"put","envelope":envelope}))
                .await
            {
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
        let chunks:Vec<(String,i64,String)>=self.db.prepare("SELECT c.file,c.idx,c.envelope FROM chunks c JOIN outbox o ON o.id=c.file WHERE c.state='queued' AND o.state='service-stored' AND o.team=?")?.query_map([team],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
        for (file, index, body) in chunks {
            let env: Value = serde_json::from_str(&body)?;
            self.rpc(
                team,
                json!({"method":"put_chunk","file":file,"index":index,"envelope":env}),
            )
            .await?;
            self.db.execute(
                "UPDATE chunks SET state='service-stored' WHERE file=? AND idx=?",
                params![file, index],
            )?;
        }
        let cursor = self.membership(team)?.cursor;
        let events = self
            .rpc(team, json!({"method":"pull","cursor":cursor}))
            .await?;
        let mut received = 0;
        for row in events.as_array().context("invalid mailbox")? {
            let env: Signed<Sealed> = serde_json::from_value(row["envelope"].clone())?;
            let seq = row["seq"].as_i64().context("missing sequence")?;
            self.receive(seq, &env)?;
            self.rpc(team, json!({"method":"ack","event":env.body.header.id}))
                .await?;
            self.db
                .execute("UPDATE teams SET cursor=? WHERE id=?", params![seq, team])?;
            received += 1;
        }
        Ok(json!({"state":"synced","received":received}))
    }
    pub fn receive(&mut self, seq: i64, env: &Signed<Sealed>) -> Result<()> {
        let t = self
            .team(&env.body.header.team)
            .context("event for a team this member does not belong to")?;
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
            "INSERT OR IGNORE INTO inbox(id,seq,event,envelope,team) VALUES(?,?,?,?,?)",
            params![
                env.body.header.id,
                seq,
                serde_json::to_string(&event)?,
                serde_json::to_string(env)?,
                t.id
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn inbox(&self) -> Result<Value> {
        self.inbox_for(None, None, false)
    }
    /// The inbox for one team, everything, or as one agent sees it: its team only, what is
    /// addressed to it or shared, optionally only what it has not marked read.
    pub fn inbox_for(
        &self,
        team: Option<&str>,
        agent: Option<&str>,
        unread_only: bool,
    ) -> Result<Value> {
        let mut team = team.map(str::to_owned);
        let (cursor, label) = match agent {
            Some(label) => {
                valid_label(label)?;
                let (cursor, agent_team): (i64, Option<String>) = self
                    .db
                    .query_row(
                        "SELECT cursor,team FROM agents WHERE label=? AND retired=0",
                        [label],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?
                    .context("unknown agent")?;
                let Some(agent_team) = agent_team else {
                    return Ok(json!([]));
                };
                if team.as_deref().is_some_and(|t| t != agent_team) {
                    return Ok(json!([]));
                }
                team = Some(agent_team);
                (if unread_only { cursor } else { 0 }, Some(label.to_owned()))
            }
            None => (0, None),
        };
        let me = self.identity.member()?.id;
        let mut q = self.db.prepare(
            "SELECT id,seq,event,envelope,dispatch,team FROM inbox WHERE seq>?1 AND (?4 IS NULL OR team=?4) AND (?2 IS NULL OR json_extract(event,'$.to_agent') IS NULL OR json_extract(event,'$.to_agent')=?2) AND (?2 IS NULL OR json_extract(event,'$.to') IS NULL OR json_extract(event,'$.to')=?3) ORDER BY team,seq",
        )?;
        let rows = q.query_map(params![cursor, label, me, team], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
            ))
        })?;
        let mut out = vec![];
        for row in rows {
            let (id, seq, event, env, dispatch, team) = row?;
            let env: Signed<Sealed> = serde_json::from_str(&env)?;
            out.push(json!({"id":id,"sequence":seq,"team":team,"sender":env.signer,"kind":env.body.header.kind,"created":env.body.header.created,"event":serde_json::from_str::<Value>(&event)?,"dispatch":dispatch}));
        }
        Ok(json!(out))
    }
    pub async fn outbox(&self, team: Option<&str>) -> Result<Value> {
        let rows: Vec<OutboxRow> = self
            .db
            .prepare("SELECT id,state,envelope,error,team FROM outbox WHERE ?1 IS NULL OR team=?1 ORDER BY rowid")?
            .query_map([team], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = vec![];
        let mut unreachable: Vec<String> = vec![];
        for (eid, state, body, error, event_team) in rows {
            let env: Signed<Sealed> = serde_json::from_str(&body)?;
            let mut item = json!({"id":eid,"state":state,"error":error,"kind":env.body.header.kind,"team":event_team});
            if state == "service-stored"
                && let Some(event_team) = &event_team
                && !unreachable.contains(event_team)
            {
                match self
                    .rpc(event_team, json!({"method":"receipts","event":eid}))
                    .await
                {
                    Ok(receipts) => {
                        if receipts["expired"] == true {
                            item["state"] = json!("expired");
                        }
                        item["receipts"] = receipts;
                    }
                    Err(e) => {
                        // Stop asking once that authority is known to be unreachable.
                        unreachable.push(event_team.clone());
                        item["receipts_error"] = json!(e.to_string());
                    }
                }
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
    pub fn share(
        &mut self,
        team: &str,
        path: &Path,
        actor: &str,
        agent: Option<String>,
    ) -> Result<String> {
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
                team,
                "file",
                actor,
                "Shared a file",
                None,
                None,
                serde_json::to_value(&manifest)?,
                agent,
                None,
            )?;
            let body: String =
                self.db
                    .query_row("SELECT envelope FROM outbox WHERE id=?", [&eid], |r| {
                        r.get(0)
                    })?;
            let base: Signed<Sealed> = serde_json::from_str(&body)?;
            let members: Vec<_> = self.team(team)?.members.values().cloned().collect();
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
    /// The team an inbox event belongs to.
    pub fn event_team(&self, event: &str) -> Result<String> {
        valid_id(event)?;
        self.db
            .query_row("SELECT team FROM inbox WHERE id=?", [event], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()?
            .flatten()
            .context("event is not in the local inbox; sync first")
    }
    pub async fn download(
        &self,
        file: &str,
        directory: &Path,
        max_chunks: Option<usize>,
    ) -> Result<Value> {
        let team_id = self.event_team(file)?;
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
                let team = self.team(&team_id)?;
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
                    self.rpc(
                        &team_id,
                        json!({"method":"get_chunk","file":file,"index":i}),
                    )
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
    /// Latest status per member and per agent in a team, with whether that member is connected.
    pub fn statuses(&self, team: &str) -> Result<Value> {
        let inbox = self.inbox_for(Some(team), None, false)?;
        let mut statuses = BTreeMap::new();
        for v in inbox.as_array().context("invalid inbox")? {
            if v["kind"] == "status" {
                let member = v["sender"].as_str().unwrap_or_default().to_owned();
                let agent = v["event"]["agent"].as_str().map(str::to_owned);
                statuses.insert((member, agent), v.clone());
            }
        }
        let t = self.team(team)?;
        let mut out = vec![];
        for ((member, agent), status) in statuses {
            out.push(json!({"member":member,"agent":agent,"status":status,"connected":t.presence.get(&member).is_some_and(|seen|now()-seen<15)}));
        }
        Ok(json!(out))
    }

    // --------------------------------------------------------------- command
    /// Actions a coding-agent session may only perform through an agent enrolled in the team.
    /// Owner commands from the shell carry no `via` and are not gated.
    const TEAM_ACTIONS: &[&str] = &[
        "list",
        "requests",
        "approve",
        "reject",
        "promote",
        "demote",
        "revoke",
        "leave",
        "invite",
        "inbox",
        "outbox",
        "sync",
        "send",
        "agent-send",
        "files",
        "share",
        "download",
        "status",
        "handoff",
        "accept-handoff",
        "worker",
    ];
    pub async fn command(&mut self, cmd: Value) -> Result<Value> {
        let action = field(&cmd, "action")?;
        if Self::TEAM_ACTIONS.contains(&action)
            && let Some(via) = cmd["via"].as_str()
        {
            let enrolled = valid_label(via)
                .ok()
                .and_then(|_| self.is_enrolled(via).ok().flatten());
            let Some(agent_team) = enrolled else {
                bail!(
                    "this workspace's agent {via} is not enrolled in a team; the user can enroll it with `whatsai agent enroll {via}`"
                );
            };
            if let Some(selector) = cmd["team"].as_str() {
                ensure!(
                    self.find_team(selector)? == agent_team,
                    "agent {via} is enrolled in a different team"
                );
            }
        }
        let team = |cmd: &Value| self.resolve_team(cmd);
        match action {
            "worker" => self.worker_command(&cmd),
            "agent" => self.agent_command(&cmd),
            "agents" => self.agents(),
            "teams" => self.teams(),
            "accept-handoff" => {
                self.accept_handoff(
                    field(&cmd, "event")?,
                    Path::new(field(&cmd, "repo")?),
                    Path::new(field(&cmd, "directory")?),
                )
                .await
            }
            "files" => {
                let scope = cmd["team"]
                    .as_str()
                    .map(|s| self.find_team(s))
                    .transpose()?;
                Ok(json!(
                    self.inbox_for(scope.as_deref(), cmd["agent"].as_str(), false)?
                        .as_array()
                        .context("invalid inbox")?
                        .iter()
                        .filter(|v| v["kind"] == "file")
                        .collect::<Vec<_>>()
                ))
            }
            "health" => Ok(
                json!({"version":VERSION,"state":self.dir,"member":self.identity.member()?,"teams":self.teams()?,"agents":self.agents()?.as_array().map(|a|a.len()).unwrap_or(0),"endpoint":self.config("endpoint")?.map(|t|serde_json::from_str::<Value>(&t)).transpose()?,"last_sync_error":self.config("last_sync_error")?,"last_peer_path":self.config("last_peer_path")?,"last_file_path":self.config("last_file_path")?}),
            ),
            "register" => Ok(json!(self.identity.member()?)),
            "create" => {
                let workspace = cmd["workspace"]
                    .as_str()
                    .or(cmd["cwd"].as_str())
                    .context("missing workspace")?;
                let result = self
                    .create(cmd["repository"].as_str(), Path::new(workspace))
                    .await?;
                // The session that founds a team from a checkout is part of it.
                if let Some(via) = cmd["via"].as_str()
                    && valid_label(via).is_ok()
                    && let Some(team) = result["team"].as_str()
                {
                    let _ = self.enroll(via, true, Some(team));
                }
                Ok(result)
            }
            "invite" => {
                self.wait_online(Duration::from_secs(5)).await;
                self.invite(&team(&cmd)?)
            }
            "join" => {
                let workspace = cmd["workspace"]
                    .as_str()
                    .or(cmd["cwd"].as_str())
                    .context("missing workspace")?;
                let result = self
                    .join(
                        cmd["key"]
                            .as_str()
                            .or(cmd["descriptor"].as_str())
                            .context("missing key")?,
                        Path::new(workspace),
                    )
                    .await?;
                // The session that brought the key in is part of the team it named.
                if let Some(via) = cmd["via"].as_str()
                    && valid_label(via).is_ok()
                    && matches!(result["state"].as_str(), Some("enrolled" | "admitted"))
                    && let Some(team) = result["team"].as_str()
                {
                    let _ = self.enroll(via, true, Some(team));
                }
                Ok(result)
            }
            "join-status" => self.join_status().await,
            "list" => Ok(json!(self.refresh(&team(&cmd)?).await?)),
            "requests" => {
                let t = team(&cmd)?;
                self.rpc(&t, json!({"method":"requests"})).await
            }
            "approve" => {
                let t = team(&cmd)?;
                self.govern(&t, "admit", field(&cmd, "member")?).await
            }
            "reject" => {
                let t = team(&cmd)?;
                self.rpc(
                    &t,
                    json!({"method":"reject","member":field(&cmd,"member")?}),
                )
                .await
            }
            "promote" | "demote" | "revoke" => {
                let t = team(&cmd)?;
                self.govern(&t, action, field(&cmd, "member")?).await
            }
            "leave" => {
                let t = team(&cmd)?;
                self.leave(&t).await
            }
            "sync" => self.sync().await,
            "inbox" => {
                let scope = cmd["team"]
                    .as_str()
                    .map(|s| self.find_team(s))
                    .transpose()?;
                self.inbox_for(
                    scope.as_deref(),
                    cmd["agent"].as_str(),
                    cmd["unread"].as_bool().unwrap_or(false),
                )
            }
            "outbox" => {
                let scope = cmd["team"]
                    .as_str()
                    .map(|s| self.find_team(s))
                    .transpose()?;
                self.outbox(scope.as_deref()).await
            }
            "send" | "agent-send" => {
                let t = team(&cmd)?;
                let eid = self.enqueue(
                    &t,
                    "message",
                    if action == "send" { "person" } else { "agent" },
                    field(&cmd, "text")?,
                    cmd["to"].as_str().map(String::from),
                    cmd["reply_to"].as_str().map(String::from),
                    Value::Null,
                    cmd["agent"].as_str().map(String::from),
                    cmd["to_agent"].as_str().map(String::from),
                )?;
                Ok(json!({"id":eid,"team":t,"state":"queued"}))
            }
            "share" => {
                let t = team(&cmd)?;
                Ok(
                    json!({"id":self.share(&t,Path::new(field(&cmd,"path")?),cmd["actor"].as_str().unwrap_or("person"),cmd["agent"].as_str().map(String::from))?,"team":t,"state":"queued"}),
                )
            }
            "download" => {
                self.download(
                    field(&cmd, "file")?,
                    Path::new(field(&cmd, "directory")?),
                    cmd["max_chunks"].as_u64().map(|n| n as usize),
                )
                .await
            }
            "status" => {
                let t = team(&cmd)?;
                if let Some(state) = cmd["state"].as_str() {
                    ensure!(
                        ["working", "blocked", "ready"].contains(&state),
                        "invalid work status"
                    );
                    let eid = self.enqueue(
                        &t,
                        "status",
                        cmd["actor"].as_str().unwrap_or("person"),
                        cmd["description"].as_str().unwrap_or(""),
                        None,
                        None,
                        json!({"state":state,"branch":cmd["branch"],"commit":cmd["commit"]}),
                        cmd["agent"].as_str().map(String::from),
                        None,
                    )?;
                    Ok(json!({"id":eid,"team":t,"state":"queued"}))
                } else {
                    self.statuses(&t)
                }
            }
            "handoff" => {
                let t = team(&cmd)?;
                let commit = field(&cmd, "commit")?;
                ensure!(
                    [40, 64].contains(&commit.len()) && hex::decode(commit).is_ok(),
                    "expected full commit hash"
                );
                ensure!(
                    self.team(&t)?.repository.is_some(),
                    "this team has no repository; handoffs need one"
                );
                let eid = self.enqueue(
                    &t,
                    "handoff",
                    cmd["actor"].as_str().unwrap_or("person"),
                    cmd["description"].as_str().unwrap_or(""),
                    None,
                    None,
                    json!({"repository":t,"commit":commit,"branch":field(&cmd,"branch")?}),
                    cmd["agent"].as_str().map(String::from),
                    None,
                )?;
                Ok(json!({"id":eid,"team":t,"state":"queued"}))
            }
            _ => bail!("unknown local operation"),
        }
    }
}

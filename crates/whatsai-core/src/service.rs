use crate::{crypto::verify, governance::*, protocol::*, storage};
use anyhow::{Context, Result, bail, ensure};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::{get, post},
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
const SCHEMA: &str = r#"
CREATE TABLE teams(id TEXT PRIMARY KEY,founder TEXT NOT NULL,history TEXT NOT NULL);
CREATE TABLE requests(team TEXT NOT NULL,member TEXT NOT NULL,body TEXT NOT NULL,state TEXT NOT NULL,expires INTEGER NOT NULL,PRIMARY KEY(team,member));
CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT UNIQUE NOT NULL,team TEXT NOT NULL,sender TEXT NOT NULL,envelope TEXT NOT NULL,expires INTEGER NOT NULL);
CREATE TABLE recipients(event TEXT NOT NULL,member TEXT NOT NULL,acked INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(event,member));
CREATE TABLE chunks(file TEXT NOT NULL,idx INTEGER NOT NULL,envelope TEXT NOT NULL,PRIMARY KEY(file,idx));
CREATE TABLE nonces(nonce TEXT PRIMARY KEY,expires INTEGER NOT NULL);
CREATE TABLE presence(team TEXT NOT NULL,member TEXT NOT NULL,seen INTEGER NOT NULL,endpoint TEXT,PRIMARY KEY(team,member));
"#;
pub struct Service {
    db: Mutex<Connection>,
    pub quota: usize,
}
impl Service {
    pub fn open(dir: &Path, quota: usize) -> Result<Self> {
        storage::private_dir(dir)?;
        Ok(Self {
            db: Mutex::new(storage::database(&dir.join("service.db"), SCHEMA)?),
            quota,
        })
    }
    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route(
                "/health",
                get(|| async { Json(json!({"version":VERSION})) }),
            )
            .route("/v1/rpc", post(rpc))
            .layer(DefaultBodyLimit::max(MAX_FRAME))
            .with_state(self)
    }
    pub fn handle(&self, request: Signed<Request>) -> Result<Value> {
        verify(&request)?;
        ensure!(
            request.body.version == VERSION,
            "unsupported protocol version"
        );
        ensure!(
            now().abs_diff(request.body.timestamp) <= 300,
            "request timestamp outside allowed window"
        );
        valid_id(&request.body.nonce)?;
        let mut db = self
            .db
            .lock()
            .map_err(|_| anyhow::anyhow!("database unavailable"))?;
        let tx = db.transaction()?;
        tx.execute("DELETE FROM nonces WHERE expires<?", [now()])?;
        tx.execute(
            "INSERT INTO nonces VALUES(?,?)",
            params![request.body.nonce, now() + 600],
        )
        .context("replayed request")?;
        let response = self.execute(&tx, &request.signer, &request.body.operation)?;
        tx.commit()?;
        Ok(response)
    }
    fn execute(&self, db: &Connection, who: &str, op: &Value) -> Result<Value> {
        let method = field(op, "method")?;
        if method == "create" {
            let record: Signed<Governance> = serde_json::from_value(op["record"].clone())?;
            ensure!(record.signer == who, "creator mismatch");
            let team = replay(std::slice::from_ref(&record), who)?;
            if let Ok(existing) = load_team(db, &team.id) {
                ensure!(
                    serde_json::to_vec(&existing.history[0])? == serde_json::to_vec(&record)?,
                    "team creation conflict"
                );
                return Ok(json!(existing));
            }
            db.execute(
                "INSERT INTO teams VALUES(?,?,?)",
                params![team.id, who, serde_json::to_string(&vec![record])?],
            )?;
            return Ok(json!(team));
        }
        let team_id = field(op, "team")?;
        let mut team = load_team(db, team_id)?;
        ensure!(!team.members.is_empty(), "team is closed");
        if method == "request_join" {
            let member: Member = serde_json::from_value(op["member"].clone())?;
            validate_member(&member)?;
            ensure!(member.id == who, "identity mismatch");
            if team.members.contains_key(who) {
                return Ok(json!({"state":"admitted","team":team}));
            }
            let existing: Option<(String, i64)> = db
                .query_row(
                    "SELECT state,expires FROM requests WHERE team=? AND member=?",
                    params![team_id, who],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((state, expires)) = existing {
                return Ok(
                    json!({"state":if expires<now() && state=="pending" {"expired"} else {&state}}),
                );
            }
            let count: i64 = db.query_row(
                "SELECT count(*) FROM requests WHERE team=? AND state='pending' AND expires>?",
                params![team_id, now()],
                |r| r.get(0),
            )?;
            ensure!(count < 100, "too many pending join requests");
            db.execute(
                "INSERT INTO requests VALUES(?,?,?,'pending',?)",
                params![team_id, who, serde_json::to_string(&member)?, now() + 86400],
            )?;
            return Ok(json!({"state":"pending"}));
        }
        if method == "join_status" {
            if team.members.contains_key(who) {
                return Ok(json!({"state":"admitted","team":team}));
            }
            let (state, expires): (String, i64) = db.query_row(
                "SELECT state,expires FROM requests WHERE team=? AND member=?",
                params![team_id, who],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            return Ok(
                json!({"state":if expires<now() && state=="pending" {"expired"} else {&state}}),
            );
        }
        ensure!(team.members.contains_key(who), "membership denied");
        match method {
            "team" => {
                db.execute("INSERT INTO presence(team,member,seen,endpoint) VALUES(?,?,?,?) ON CONFLICT(team,member) DO UPDATE SET seen=excluded.seen,endpoint=COALESCE(excluded.endpoint,presence.endpoint)",params![team_id,who,now(),op.get("endpoint").map(serde_json::to_string).transpose()?])?;
                team = load_team(db, team_id)?;
                Ok(json!(team))
            }
            "requests" => {
                ensure!(team.admins.contains(&who.to_owned()), "admin required");
                let mut q = db.prepare(
                    "SELECT body,state,expires FROM requests WHERE team=? ORDER BY expires",
                )?;
                let rows = q.query_map([team_id], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                })?;
                let mut out = vec![];
                for row in rows {
                    let (body, state, expires) = row?;
                    out.push(json!({"member":serde_json::from_str::<Value>(&body)?,"state":if state=="pending" && expires<now(){"expired"}else{&state},"expires":expires}));
                }
                Ok(json!(out))
            }
            "reject" => {
                ensure!(team.admins.contains(&who.to_owned()), "admin required");
                let n=db.execute("UPDATE requests SET state='rejected' WHERE team=? AND member=? AND state='pending'",params![team_id,field(op,"member")?])?;
                ensure!(n == 1, "no pending request");
                Ok(json!({"state":"rejected"}))
            }
            "govern" => {
                let record: Signed<Governance> = serde_json::from_value(op["record"].clone())?;
                ensure!(record.signer == who, "signer mismatch");
                if record.body.action == "admit" {
                    let m = record
                        .body
                        .member
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("missing member"))?;
                    let (body, state, expires): (String, String, i64) = db.query_row(
                        "SELECT body,state,expires FROM requests WHERE team=? AND member=?",
                        params![team_id, m.id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?;
                    ensure!(
                        state == "pending" && expires >= now(),
                        "request is not pending or expired"
                    );
                    ensure!(
                        serde_json::from_str::<Member>(&body)? == *m,
                        "requested keys changed"
                    );
                }
                let mut log = team.history.clone();
                log.push(record.clone());
                let next = replay(&log, &team.founder)?;
                db.execute(
                    "UPDATE teams SET history=? WHERE id=?",
                    params![serde_json::to_string(&log)?, team_id],
                )?;
                if record.body.action == "admit" {
                    db.execute(
                        "UPDATE requests SET state='admitted' WHERE team=? AND member=?",
                        params![team_id, record.body.member.unwrap().id],
                    )?;
                }
                if next.members.is_empty() {
                    db.execute(
                        "DELETE FROM chunks WHERE file IN (SELECT id FROM events WHERE team=?)",
                        [team_id],
                    )?;
                    db.execute("DELETE FROM recipients WHERE event IN (SELECT id FROM events WHERE team=?)",[team_id])?;
                    db.execute("DELETE FROM events WHERE team=?", [team_id])?;
                }
                Ok(json!(next))
            }
            "put" => {
                let envelope: Signed<Sealed> = serde_json::from_value(op["envelope"].clone())?;
                validate_envelope(&envelope, &team, who)?;
                let eid = &envelope.body.header.id;
                let serialized = serde_json::to_string(&envelope)?;
                if let Some((seq, old, expires)) = db
                    .query_row(
                        "SELECT seq,envelope,expires FROM events WHERE id=?",
                        [eid],
                        |r| {
                            Ok((
                                r.get::<_, i64>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, i64>(2)?,
                            ))
                        },
                    )
                    .optional()?
                {
                    ensure!(old == serialized, "event ID conflict");
                    ensure!(expires > now(), "event expired");
                    return Ok(json!({"seq":seq,"state":"service-stored"}));
                }
                let used: i64 = db.query_row(
                    "SELECT COALESCE(sum(length(envelope)),0) FROM events",
                    [],
                    |r| r.get(0),
                )?;
                let chunks: i64 = db.query_row(
                    "SELECT COALESCE(sum(length(envelope)),0) FROM chunks",
                    [],
                    |r| r.get(0),
                )?;
                ensure!(
                    (used + chunks) as usize + serialized.len() <= self.quota,
                    "storage quota exceeded"
                );
                db.execute(
                    "INSERT INTO events(id,team,sender,envelope,expires) VALUES(?,?,?,?,?)",
                    params![eid, team_id, who, serialized, now() + RETENTION],
                )?;
                let seq = db.last_insert_rowid();
                for r in &envelope.body.header.recipients {
                    db.execute(
                        "INSERT INTO recipients(event,member) VALUES(?,?)",
                        params![eid, r],
                    )?;
                }
                Ok(json!({"seq":seq,"state":"service-stored"}))
            }
            "pull" => {
                let cursor = op["cursor"].as_i64().unwrap_or(0);
                let mut q=db.prepare("SELECT e.seq,e.envelope FROM events e JOIN recipients r ON r.event=e.id WHERE e.team=? AND r.member=? AND e.seq>? AND e.expires>? ORDER BY e.seq LIMIT 100")?;
                let rows = q.query_map(params![team_id, who, cursor, now()], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
                })?;
                let mut out = vec![];
                for r in rows {
                    let (seq, envelope) = r?;
                    out.push(
                        json!({"seq":seq,"envelope":serde_json::from_str::<Value>(&envelope)?}),
                    );
                }
                Ok(json!(out))
            }
            "authorize" => {
                let event = field(op, "event")?;
                let receiver = field(op, "recipient")?;
                ensure!(team.members.contains_key(receiver), "recipient revoked");
                let (sender, envelope, expires) = event_row(db, team_id, event)?;
                ensure!(
                    sender == who || receiver == who,
                    "not authorized for operation"
                );
                ensure!(expires > now(), "event expired");
                let env: Signed<Sealed> = serde_json::from_str(&envelope)?;
                ensure!(
                    env.body.header.recipients.contains(&receiver.to_owned()),
                    "not original recipient"
                );
                let seq: i64 =
                    db.query_row("SELECT seq FROM events WHERE id=?", [event], |r| r.get(0))?;
                Ok(
                    json!({"authorized":true,"digest":digest(envelope.as_bytes()),"revision":team.revision,"seq":seq}),
                )
            }
            "ack" => {
                let eid = field(op, "event")?;
                let (_, _, expires) = event_row(db, team_id, eid)?;
                ensure!(expires > now(), "event expired");
                let n = db.execute(
                    "UPDATE recipients SET acked=1 WHERE event=? AND member=?",
                    params![eid, who],
                )?;
                ensure!(n == 1, "not recipient");
                Ok(json!({"state":"recipient-delivered"}))
            }
            "receipts" => {
                let eid = field(op, "event")?;
                let (sender, _, expires) = event_row(db, team_id, eid)?;
                ensure!(sender == who, "sender required");
                let mut q = db.prepare("SELECT member,acked FROM recipients WHERE event=?")?;
                let rows = q.query_map([eid], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, bool>(1)?))
                })?;
                let mut receipts = BTreeMap::new();
                for r in rows {
                    let (k, v) = r?;
                    receipts.insert(k, v);
                }
                Ok(json!({"expired":expires<=now(),"recipients":receipts}))
            }
            "put_chunk" | "get_chunk" => {
                let file = field(op, "file")?;
                let idx = op["index"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("missing chunk index"))?;
                ensure!(idx < 32, "invalid chunk index");
                let idx = idx as i64;
                let (sender, manifest, expires) = event_row(db, team_id, file)?;
                ensure!(expires > now(), "file expired");
                let env: Signed<Sealed> = serde_json::from_str(&manifest)?;
                ensure!(
                    env.body.header.kind == "file"
                        && env.body.header.recipients.contains(&who.to_owned()),
                    "not a file recipient"
                );
                if method == "put_chunk" {
                    ensure!(sender == who, "sender required");
                    let chunk: Signed<Sealed> = serde_json::from_value(op["envelope"].clone())?;
                    verify(&chunk)?;
                    ensure!(
                        chunk.signer == who
                            && chunk.body.header.sender == who
                            && chunk.body.header.team == team_id
                            && chunk.body.header.id == file
                            && chunk.body.header.kind == format!("chunk/{idx}")
                            && chunk.body.header.recipients == env.body.header.recipients,
                        "invalid chunk binding"
                    );
                    let body = serde_json::to_string(&chunk)?;
                    ensure!(body.len() < 2 * CHUNK_SIZE, "chunk too large");
                    if let Some(old) = db
                        .query_row(
                            "SELECT envelope FROM chunks WHERE file=? AND idx=?",
                            params![file, idx],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()?
                    {
                        ensure!(old == body, "chunk conflict");
                        return Ok(json!({"stored":true}));
                    }
                    let used:i64=db.query_row("SELECT (SELECT COALESCE(sum(length(envelope)),0) FROM chunks)+(SELECT COALESCE(sum(length(envelope)),0) FROM events)",[],|r|r.get(0))?;
                    ensure!(
                        used as usize + body.len() <= self.quota,
                        "storage quota exceeded"
                    );
                    db.execute("INSERT INTO chunks VALUES(?,?,?)", params![file, idx, body])?;
                    db.execute(
                        "UPDATE events SET expires=MAX(expires,?) WHERE id=?",
                        params![now() + RETENTION, file],
                    )?;
                    Ok(json!({"stored":true}))
                } else {
                    let body: String = db
                        .query_row(
                            "SELECT envelope FROM chunks WHERE file=? AND idx=?",
                            params![file, idx],
                            |r| r.get(0),
                        )
                        .context("chunk not available yet")?;
                    Ok(serde_json::from_str(&body)?)
                }
            }
            _ => bail!("unknown operation"),
        }
    }
}
use std::collections::BTreeMap;
fn load_team(db: &Connection, id: &str) -> Result<Team> {
    let (founder, history): (String, String) = db
        .query_row("SELECT founder,history FROM teams WHERE id=?", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .context("unknown team")?;
    let mut t = replay(
        &serde_json::from_str::<Vec<Signed<Governance>>>(&history)?,
        &founder,
    )?;
    let mut q = db.prepare("SELECT member,seen,endpoint FROM presence WHERE team=?")?;
    let rows = q.query_map([id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (member, seen, endpoint) = row?;
        if t.members.contains_key(&member) {
            t.presence.insert(member.clone(), seen);
            if let Some(e) = endpoint {
                t.endpoints.insert(member, serde_json::from_str(&e)?);
            }
        }
    }
    Ok(t)
}
fn event_row(db: &Connection, team: &str, id: &str) -> Result<(String, String, i64)> {
    Ok(db.query_row(
        "SELECT sender,envelope,expires FROM events WHERE id=? AND team=?",
        params![id, team],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}
fn validate_envelope(e: &Signed<Sealed>, team: &Team, who: &str) -> Result<()> {
    verify(e)?;
    let h = &e.body.header;
    valid_id(&h.id)?;
    ensure!(
        e.signer == who && h.sender == who && h.team == team.id && h.version == VERSION,
        "invalid envelope sender/team/version"
    );
    ensure!(
        ["message", "status", "handoff", "file"].contains(&h.kind.as_str()),
        "invalid event kind"
    );
    let recipients: Vec<_> = team.members.keys().cloned().collect();
    ensure!(
        h.recipients == recipients && e.body.keys.keys().cloned().collect::<Vec<_>>() == recipients,
        "recipient roster changed; refresh before sending"
    );
    ensure!(e.body.ciphertext.len() <= 768 * 1024, "event too large");
    Ok(())
}
pub fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing {key}"))
}
async fn rpc(
    State(service): State<Arc<Service>>,
    Json(req): Json<Signed<Request>>,
) -> (StatusCode, Json<Value>) {
    let result = tokio::task::spawn_blocking(move || service.handle(req)).await;
    match result {
        Ok(Ok(v)) => (StatusCode::OK, Json(json!({"ok":true,"result":v}))),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"ok":false,"error":e.to_string()})),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok":false,"error":"service unavailable"})),
        ),
    }
}
pub async fn serve(listener: tokio::net::TcpListener, service: Arc<Service>) -> Result<()> {
    axum::serve(listener, service.router())
        .with_graceful_shutdown(crate::daemon::shutdown_signal())
        .await?;
    Ok(())
}

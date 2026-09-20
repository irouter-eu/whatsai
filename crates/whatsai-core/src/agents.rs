//! Durable agents and ephemeral sessions under one member.
//!
//! A member is a person on a device. An agent is what teammates address: `harness@workspace`,
//! created the first time a coding-agent session attaches from that harness in that checkout,
//! and kept until retired so messages sent while nobody is there wait for the next session.
//! A session is one running harness process holding a lease on an agent.
//!
//! Attaching is local bookkeeping. An agent takes part in one team only once it is enrolled
//! there: automatically when its checkout matches a team, by Git origin for repository-bound
//! teams or by exact path for path-bound ones (unless auto-enroll is off), otherwise on the
//! owner's say-so. Sessions of unenrolled agents cannot read, send, or
//! see the join key. Publishing, which lets the team see and address the agent, is a further
//! explicit step, with one opt-in for checkouts of the team's own repository.
use crate::{client::Client, protocol::*, service::field};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::path::Path;

/// A session without a heartbeat for this long is gone.
pub const SESSION_TTL: i64 = 45;

impl Client {
    /// Attach a session to the agent for `harness` in `workspace`, creating the agent if needed.
    pub fn attach(
        &self,
        harness: &str,
        workspace: &Path,
        session: Option<&str>,
        pid: Option<i64>,
        repository: Option<&str>,
    ) -> Result<Value> {
        valid_harness(harness)?;
        let workspace = canonical_workspace(workspace)?;
        let repository = repository
            .map(str::to_owned)
            .or_else(|| detect_repository(Path::new(&workspace)));
        let existing: Option<String> = self
            .db
            .query_row(
                "SELECT label FROM agents WHERE harness=? AND workspace=?",
                params![harness, workspace],
                |r| r.get(0),
            )
            .optional()?;
        let label = match existing {
            Some(label) => {
                self.db.execute(
                    "UPDATE agents SET last_seen=?,retired=0,repository=COALESCE(?,repository) WHERE label=?",
                    params![now(), repository, label],
                )?;
                label
            }
            None => {
                let label = self.free_label(harness, &workspace)?;
                self.db.execute(
                    "INSERT INTO agents(label,harness,workspace,repository,created,last_seen) VALUES(?,?,?,?,?,?)",
                    params![label, harness, workspace, repository, now(), now()],
                )?;
                label
            }
        };
        // A checkout that matches one of our teams is part of it unless the owner says
        // otherwise; publishing it too is a separate opt-in.
        if let Some(team) = self.match_workspace(Path::new(&workspace))? {
            if self.config("auto_enroll")?.as_deref() != Some("off") {
                self.db.execute(
                    "UPDATE agents SET enrolled=1,team=? WHERE label=?",
                    params![team, label],
                )?;
            }
            if self.config("auto_publish")?.as_deref() == Some("team-repo") {
                self.db.execute(
                    "UPDATE agents SET published=1,enrolled=1,team=? WHERE label=?",
                    params![team, label],
                )?;
            }
        }
        let lease = id();
        self.db.execute(
            "INSERT INTO sessions(lease,agent,session,pid,started,heartbeat) VALUES(?,?,?,?,?,?)",
            params![lease, label, session, pid, now(), now()],
        )?;
        Ok(json!({"agent":self.agent(&label)?,"lease":lease}))
    }
    pub fn heartbeat(&self, lease: &str) -> Result<Value> {
        valid_id(lease)?;
        let n = self.db.execute(
            "UPDATE sessions SET heartbeat=? WHERE lease=?",
            params![now(), lease],
        )?;
        ensure!(n == 1, "unknown session lease; attach again");
        self.db.execute(
            "UPDATE agents SET last_seen=? WHERE label=(SELECT agent FROM sessions WHERE lease=?)",
            params![now(), lease],
        )?;
        Ok(json!({"lease":lease,"state":"alive"}))
    }
    pub fn detach(&self, lease: &str) -> Result<Value> {
        valid_id(lease)?;
        self.db
            .execute("DELETE FROM sessions WHERE lease=?", [lease])?;
        Ok(json!({"lease":lease,"state":"detached"}))
    }
    /// Drop sessions whose harness process stopped heartbeating.
    pub fn expire_sessions(&self) -> Result<usize> {
        Ok(self.db.execute(
            "DELETE FROM sessions WHERE heartbeat<?",
            [now() - SESSION_TTL],
        )?)
    }
    /// The agent for a harness in a workspace, if one exists; never creates one.
    pub fn resolve_agent(&self, harness: &str, workspace: &Path) -> Result<Option<String>> {
        valid_harness(harness)?;
        let workspace = canonical_workspace(workspace)?;
        Ok(self
            .db
            .query_row(
                "SELECT label FROM agents WHERE harness=? AND workspace=? AND retired=0",
                params![harness, workspace],
                |r| r.get(0),
            )
            .optional()?)
    }
    pub fn agent(&self, label: &str) -> Result<Value> {
        self.expire_sessions()?;
        let row = self
            .db
            .query_row(
                "SELECT harness,workspace,repository,created,last_seen,retired,worker,cursor,(SELECT count(*) FROM sessions WHERE agent=label),published,enrolled,team FROM agents WHERE label=?",
                [label],
                |r| {
                    Ok(json!({
                        "label":label,
                        "harness":r.get::<_,String>(0)?,
                        "workspace":r.get::<_,String>(1)?,
                        "repository":r.get::<_,Option<String>>(2)?,
                        "created":r.get::<_,i64>(3)?,
                        "last_seen":r.get::<_,i64>(4)?,
                        "retired":r.get::<_,i64>(5)?==1,
                        "worker":r.get::<_,Option<String>>(6)?.map(|w|serde_json::from_str::<Value>(&w)).transpose().unwrap_or_default(),
                        "cursor":r.get::<_,i64>(7)?,
                        "sessions":r.get::<_,i64>(8)?,
                        "published":r.get::<_,i64>(9)?==1,
                        "enrolled":r.get::<_,i64>(10)?==1,
                        "team":r.get::<_,Option<String>>(11)?,
                    }))
                },
            )
            .optional()?
            .context("unknown agent")?;
        let mut row = row;
        row["online"] = json!(row["sessions"].as_i64().unwrap_or(0) > 0);
        row["team_name"] = row["team"]
            .as_str()
            .and_then(|t| self.team(t).ok())
            .map(|t| json!(t.workspace))
            .unwrap_or(Value::Null);
        Ok(row)
    }
    pub fn agents(&self) -> Result<Value> {
        self.expire_sessions()?;
        let labels: Vec<String> = self
            .db
            .prepare("SELECT label FROM agents ORDER BY retired,label")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut out = vec![];
        for label in labels {
            let mut a = self.agent(&label)?;
            a["unread"] = self.unread(&label)?;
            out.push(a);
        }
        Ok(json!(out))
    }
    /// What the team gets to see: only published agents, as labels and workspace names, never
    /// local paths.
    pub fn agent_presence(&self, team: &str) -> Result<Value> {
        self.expire_sessions()?;
        let mut q = self.db.prepare(
            "SELECT label,harness,workspace,repository,last_seen,(SELECT count(*) FROM sessions WHERE agent=label) FROM agents WHERE retired=0 AND published=1 AND enrolled=1 AND team=? ORDER BY label",
        )?;
        let rows = q.query_map([team], |r| {
            Ok(json!({
                "label":r.get::<_,String>(0)?,
                "harness":r.get::<_,String>(1)?,
                "workspace":Path::new(&r.get::<_,String>(2)?).file_name().map(|n|n.to_string_lossy().into_owned()),
                "repository":r.get::<_,Option<String>>(3)?,
                "last_seen":r.get::<_,i64>(4)?,
                "online":r.get::<_,i64>(5)?>0,
            }))
        })?;
        Ok(json!(rows.collect::<rusqlite::Result<Vec<_>>>()?))
    }
    /// Publishing is the one act that lets the team see and address an agent; it implies
    /// taking part, so it enrolls too when the team is unambiguous.
    pub fn publish(&self, label: &str, published: bool) -> Result<Value> {
        valid_label(label)?;
        let n = if published {
            if self.is_enrolled(label)?.is_none() {
                self.enroll(label, true, None)?;
            }
            self.db.execute(
                "UPDATE agents SET published=1 WHERE label=? AND retired=0",
                [label],
            )?
        } else {
            self.db.execute(
                "UPDATE agents SET published=0 WHERE label=? AND retired=0",
                [label],
            )?
        };
        ensure!(n == 1, "unknown or retired agent");
        self.agent(label)
    }
    /// Enrolment is what lets an agent's sessions take part in a team at all. The team is
    /// given, matched from the agent's workspace, or the only one there is.
    pub fn enroll(&self, label: &str, enrolled: bool, team: Option<&str>) -> Result<Value> {
        valid_label(label)?;
        let n = if enrolled {
            let info = self.agent(label)?;
            let team = match team {
                Some(selector) => self.find_team(selector)?,
                None => {
                    let workspace = info["workspace"].as_str().unwrap_or_default();
                    match self.match_workspace(Path::new(workspace))? {
                        Some(t) => t,
                        None => {
                            let all = self.memberships()?;
                            ensure!(
                                all.len() == 1,
                                "say which team: `whatsai agent enroll {label} --team WORKSPACE`"
                            );
                            all[0].id.clone()
                        }
                    }
                }
            };
            self.db.execute(
                "UPDATE agents SET enrolled=1,team=? WHERE label=? AND retired=0",
                params![team, label],
            )?
        } else {
            self.db.execute(
                "UPDATE agents SET enrolled=0,published=0,team=NULL WHERE label=? AND retired=0",
                [label],
            )?
        };
        ensure!(n == 1, "unknown or retired agent");
        self.agent(label)
    }
    /// Enroll every attached agent whose checkout matches `team`, after joining it.
    pub fn enroll_workspace(&self, team: &str) -> Result<usize> {
        if self.config("auto_enroll")?.as_deref() == Some("off") {
            return Ok(0);
        }
        let candidates: Vec<(String, String)> = self
            .db
            .prepare("SELECT label,workspace FROM agents WHERE retired=0 AND team IS NULL")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let mut n = 0;
        for (label, workspace) in candidates {
            if self.match_workspace(Path::new(&workspace))?.as_deref() == Some(team) {
                self.db.execute(
                    "UPDATE agents SET enrolled=1,team=? WHERE label=?",
                    params![team, label],
                )?;
                n += 1;
            }
        }
        Ok(n)
    }
    /// Enroll every agent attached from exactly this directory into `team`, regardless of what
    /// the directory would match on its own. Used when the owner deliberately brings a
    /// workspace into a team.
    pub fn enroll_path(&self, team: &str, workspace: &Path) -> Result<Vec<String>> {
        let canonical = std::fs::canonicalize(workspace)
            .context("workspace does not exist")?
            .to_string_lossy()
            .into_owned();
        let labels: Vec<String> = self
            .db
            .prepare("SELECT label FROM agents WHERE retired=0 AND workspace=? ORDER BY label")?
            .query_map([&canonical], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for label in &labels {
            self.db.execute(
                "UPDATE agents SET enrolled=1,team=? WHERE label=?",
                params![team, label],
            )?;
        }
        Ok(labels)
    }
    /// A deliberate act by the owner (creating a team from a directory, joining one with a key)
    /// makes that directory's agents visible to the team: enrolled and published.
    pub fn publish_path(&self, team: &str, workspace: &Path) -> Result<Vec<String>> {
        let labels = self.enroll_path(team, workspace)?;
        for label in &labels {
            self.db
                .execute("UPDATE agents SET published=1 WHERE label=?", [label])?;
        }
        Ok(labels)
    }
    /// The team a session acting through `label` may touch, if any.
    pub fn is_enrolled(&self, label: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .query_row(
                "SELECT team FROM agents WHERE label=? AND retired=0 AND enrolled=1",
                [label],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }
    /// Retiring keeps history but stops the agent being offered or addressed by the team.
    pub fn retire(&self, label: &str) -> Result<Value> {
        valid_label(label)?;
        let n = self.db.execute(
            "UPDATE agents SET retired=1,worker=NULL,published=0,enrolled=0,team=NULL WHERE label=?",
            [label],
        )?;
        ensure!(n == 1, "unknown agent");
        self.db
            .execute("DELETE FROM sessions WHERE agent=?", [label])?;
        self.agent(label)
    }
    /// Move an agent to a new checkout so its label, queue and budgets follow the work.
    pub fn adopt(&self, label: &str, workspace: &Path) -> Result<Value> {
        valid_label(label)?;
        let workspace = canonical_workspace(workspace)?;
        let harness: String = self
            .db
            .query_row("SELECT harness FROM agents WHERE label=?", [label], |r| {
                r.get(0)
            })
            .optional()?
            .context("unknown agent")?;
        // A placeholder auto-created at the destination gives way; its label was never shared.
        let placeholder: Option<String> = self
            .db
            .query_row(
                "SELECT label FROM agents WHERE harness=? AND workspace=? AND label!=?",
                params![harness, workspace, label],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(placeholder) = placeholder {
            self.db
                .execute("DELETE FROM agents WHERE label=?", [&placeholder])?;
        }
        let repository = detect_repository(Path::new(&workspace));
        self.db.execute(
            "UPDATE agents SET workspace=?,repository=COALESCE(?,repository),retired=0,last_seen=? WHERE label=?",
            params![workspace, repository, now(), label],
        )?;
        self.agent(label)
    }
    /// Unread counts for one agent in its team: addressed to it explicitly, and shared (to the
    /// member or to everyone) that it has not marked read yet. Own messages never count.
    pub fn unread(&self, label: &str) -> Result<Value> {
        let (cursor, team): (i64, Option<String>) = self
            .db
            .query_row(
                "SELECT cursor,team FROM agents WHERE label=? AND retired=0",
                [label],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .context("unknown agent")?;
        let Some(team) = team else {
            return Ok(
                json!({"agent":label,"addressed":0,"shared":0,"cursor":cursor,"enrolled":false}),
            );
        };
        let me = self.identity.member()?.id;
        let count = |sql: &str| -> Result<i64> {
            Ok(self
                .db
                .query_row(sql, params![cursor, me, label, team], |r| r.get(0))?)
        };
        let addressed = count(
            "SELECT count(*) FROM inbox WHERE seq>?1 AND team=?4 AND json_extract(envelope,'$.signer')!=?2 AND json_extract(event,'$.to')=?2 AND json_extract(event,'$.to_agent')=?3",
        )?;
        let shared = count(
            "SELECT count(*) FROM inbox WHERE seq>?1 AND team=?4 AND json_extract(envelope,'$.signer')!=?2 AND json_extract(event,'$.to_agent') IS NULL AND (json_extract(event,'$.to') IS NULL OR json_extract(event,'$.to')=?2) AND ?3=?3",
        )?;
        Ok(
            json!({"agent":label,"addressed":addressed,"shared":shared,"cursor":cursor,"enrolled":true,"team":team}),
        )
    }
    pub fn mark_read(&self, label: &str) -> Result<Value> {
        let team = self.is_enrolled(label)?;
        let latest: i64 = self.db.query_row(
            "SELECT COALESCE(max(seq),0) FROM inbox WHERE ?1 IS NULL OR team=?1",
            [team],
            |r| r.get(0),
        )?;
        let n = self.db.execute(
            "UPDATE agents SET cursor=? WHERE label=?",
            params![latest, label],
        )?;
        ensure!(n == 1, "unknown agent");
        self.unread(label)
    }
    /// `harness@name`, with a numeric suffix when another workspace already took the name.
    fn free_label(&self, harness: &str, workspace: &str) -> Result<String> {
        let mut name: String = Path::new(workspace)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".into())
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                    c
                } else {
                    '-'
                }
            })
            .take(40)
            .collect();
        if name.is_empty() {
            name = "workspace".into();
        }
        for n in 1..1000 {
            let candidate = if n == 1 {
                format!("{harness}@{name}")
            } else {
                format!("{harness}@{name}-{n}")
            };
            let taken: bool = self.db.query_row(
                "SELECT count(*) FROM agents WHERE label=?",
                [&candidate],
                |r| r.get::<_, i64>(0),
            )? > 0;
            if !taken {
                valid_label(&candidate)?;
                return Ok(candidate);
            }
        }
        anyhow::bail!("too many agents named {name}")
    }
    pub fn agent_command(&self, cmd: &Value) -> Result<Value> {
        match field(cmd, "operation")? {
            "attach" => self.attach(
                field(cmd, "harness")?,
                Path::new(field(cmd, "workspace")?),
                cmd["session"].as_str(),
                cmd["pid"].as_i64(),
                cmd["repository"].as_str(),
            ),
            "heartbeat" => self.heartbeat(field(cmd, "lease")?),
            "detach" => self.detach(field(cmd, "lease")?),
            "publish" => self.publish(&self.label_from(cmd)?, true),
            "unpublish" => self.publish(&self.label_from(cmd)?, false),
            "enroll" => self.enroll(&self.label_from(cmd)?, true, cmd["team"].as_str()),
            "unenroll" => self.enroll(&self.label_from(cmd)?, false, None),
            "auto-enroll" => {
                let mode = field(cmd, "mode")?;
                ensure!(
                    ["off", "team-repo"].contains(&mode),
                    "auto-enroll mode is off or team-repo"
                );
                self.set("auto_enroll", mode)?;
                Ok(json!({"auto_enroll":mode}))
            }
            "auto-publish" => {
                let mode = field(cmd, "mode")?;
                ensure!(
                    ["off", "team-repo"].contains(&mode),
                    "auto-publish mode is off or team-repo"
                );
                self.set("auto_publish", mode)?;
                Ok(json!({"auto_publish":mode}))
            }
            "retire" => self.retire(field(cmd, "agent")?),
            "adopt" => self.adopt(field(cmd, "agent")?, Path::new(field(cmd, "workspace")?)),
            "show" => self.agent(field(cmd, "agent")?),
            "unread" => self.unread(&self.label_from(cmd)?),
            "mark-read" => self.mark_read(&self.label_from(cmd)?),
            "resolve" => Ok(
                json!({"agent":self.resolve_agent(field(cmd,"harness")?,Path::new(field(cmd,"workspace")?))?}),
            ),
            other => anyhow::bail!("unknown agent operation {other}"),
        }
    }
    /// An agent named by label, or resolved from harness and workspace without creating it.
    pub fn label_from(&self, cmd: &Value) -> Result<String> {
        if let Some(label) = cmd["agent"].as_str() {
            valid_label(label)?;
            return Ok(label.into());
        }
        self.resolve_agent(field(cmd, "harness")?, Path::new(field(cmd, "workspace")?))?
            .context("no agent for this harness in this workspace yet")
    }
}
fn canonical_workspace(path: &Path) -> Result<String> {
    let canonical = std::fs::canonicalize(path).context("workspace does not exist")?;
    ensure!(canonical.is_dir(), "workspace is not a directory");
    Ok(canonical.to_string_lossy().into_owned())
}
/// The checkout's origin, used to match teams and to show which agents are on a repository.
/// Git is optional: no git, or no remote, simply means none.
pub fn detect_repository(workspace: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["-C"])
        .arg(workspace)
        .args(["remote", "get-url", "origin"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let remote = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    validate_remote(&remote).ok().map(|_| remote)
}

//! Automatic replies: one optional worker per agent, answering messages addressed to that agent.
use crate::{client::Client, protocol::*, service::field};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::AsyncWriteExt;
#[derive(Clone, Serialize, Deserialize)]
pub struct Binding {
    pub harness: String,
    pub cwd: String,
    pub adapter: String,
    pub enabled: bool,
    pub limit: i64,
    pub timeout_secs: u64,
    pub session: Option<String>,
    /// Also answer messages sent to the member with no agent named.
    #[serde(default)]
    pub default: bool,
}
#[derive(Clone)]
pub struct Work {
    pub event: String,
    pub team: String,
    pub sender: String,
    pub sender_agent: Option<String>,
    pub agent: String,
    pub binding: Binding,
    pub prompt: String,
}
impl Client {
    fn binding(&self, agent: &str) -> Result<Option<Binding>> {
        let raw: Option<Option<String>> = self
            .db
            .query_row("SELECT worker FROM agents WHERE label=?", [agent], |r| {
                r.get(0)
            })
            .optional()?;
        match raw {
            None => anyhow::bail!("unknown agent {agent}"),
            Some(None) => Ok(None),
            Some(Some(json)) => Ok(Some(serde_json::from_str(&json)?)),
        }
    }
    fn save_binding(&self, agent: &str, binding: Option<&Binding>) -> Result<()> {
        let json = binding.map(serde_json::to_string).transpose()?;
        let n = self.db.execute(
            "UPDATE agents SET worker=? WHERE label=? AND retired=0",
            params![json, agent],
        )?;
        ensure!(n == 1, "unknown or retired agent {agent}");
        Ok(())
    }
    pub fn worker_command(&self, cmd: &Value) -> Result<Value> {
        let op = field(cmd, "operation")?;
        if op == "status" {
            let mut q = self.db.prepare(
                "SELECT label,worker FROM agents WHERE worker IS NOT NULL ORDER BY label",
            )?;
            let rows = q.query_map([], |r| {
                Ok(json!({"agent":r.get::<_,String>(0)?,"binding":serde_json::from_str::<Value>(&r.get::<_,String>(1)?).unwrap_or_default()}))
            })?;
            return Ok(
                json!({"workers":rows.collect::<rusqlite::Result<Vec<_>>>()?,"last_error":self.config("worker_error")?}),
            );
        }
        let agent = field(cmd, "agent")?;
        valid_label(agent)?;
        let info = self.agent(agent)?;
        ensure!(info["retired"] != true, "agent is retired");
        ensure!(
            info["enrolled"] == true,
            "agent {agent} is not enrolled in the team; enroll it before binding a worker"
        );
        match op {
            "bind" => {
                let harness = cmd["harness"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| info["harness"].as_str().unwrap_or_default().to_owned());
                ensure!(
                    ["codex", "claude"].contains(&harness.as_str()),
                    "unsupported worker harness {harness}; bind with --harness codex or claude"
                );
                let cwd = std::fs::canonicalize(
                    cmd["cwd"]
                        .as_str()
                        .unwrap_or_else(|| info["workspace"].as_str().unwrap_or_default()),
                )?;
                ensure!(cwd.is_dir(), "worker cwd is not a directory");
                let adapter = std::fs::canonicalize(field(cmd, "adapter")?)?;
                ensure!(
                    adapter.is_file(),
                    "worker adapter is missing; build adapters first"
                );
                self.save_binding(
                    agent,
                    Some(&Binding {
                        harness,
                        cwd: cwd.to_string_lossy().into(),
                        adapter: adapter.to_string_lossy().into(),
                        enabled: false,
                        limit: cmd["limit"].as_i64().unwrap_or(3),
                        timeout_secs: cmd["timeout_secs"].as_u64().unwrap_or(120),
                        session: None,
                        default: cmd["default"].as_bool().unwrap_or(false),
                    }),
                )?;
            }
            "enable" | "pause" => {
                let mut b = self
                    .binding(agent)?
                    .context("bind a worker to this agent first")?;
                b.enabled = op == "enable";
                self.save_binding(agent, Some(&b))?;
            }
            "unbind" => self.save_binding(agent, None)?,
            "reset" => {
                let root = field(cmd, "root")?;
                valid_id(root)?;
                self.db.execute(
                    "DELETE FROM budgets WHERE agent=? AND root=?",
                    params![agent, root],
                )?;
                self.db.execute("UPDATE inbox SET dispatch='pending' WHERE dispatch='budget-exhausted' AND json_extract(event,'$.root')=?",[root])?;
            }
            other => anyhow::bail!("unknown worker operation {other}"),
        }
        Ok(
            json!({"agent":agent,"binding":self.binding(agent)?.map(serde_json::to_value).transpose()?,"last_error":self.config("worker_error")?}),
        )
    }
    /// Claim the next message for any enabled worker, one at a time, charging that agent's budget.
    pub fn claim_work(&mut self) -> Result<Option<Work>> {
        let me = self.identity.member()?.id;
        let bound: Vec<(String, String, String)> = self
            .db
            .prepare("SELECT label,worker,team FROM agents WHERE worker IS NOT NULL AND retired=0 AND enrolled=1 AND team IS NOT NULL ORDER BY label")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?;
        for (agent, raw, team_id) in bound {
            let binding: Binding = serde_json::from_str(&raw)?;
            if !binding.enabled {
                continue;
            }
            let Ok(team) = self.team(&team_id) else {
                continue;
            };
            if !team.members.contains_key(&me) {
                continue;
            }
            let row:Option<(String,String,String)>=self.db.query_row(
                "SELECT id,event,envelope FROM inbox WHERE dispatch='pending' AND team=?4 AND json_extract(event,'$.to')=?1 AND (json_extract(event,'$.to_agent')=?2 OR (?3 AND json_extract(event,'$.to_agent') IS NULL)) ORDER BY seq LIMIT 1",
                params![me,agent,binding.default,team_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            let Some((eid, text, envelope)) = row else {
                continue;
            };
            let event: Event = serde_json::from_str(&text)?;
            let env: Signed<Sealed> = serde_json::from_str(&envelope)?;
            if env.signer == me
                || !team.members.contains_key(&env.signer)
                || env.body.header.kind != "message"
            {
                self.db
                    .execute("UPDATE inbox SET dispatch='ignored' WHERE id=?", [eid])?;
                continue;
            }
            let tx = self.db.transaction()?;
            let used: i64 = tx
                .query_row(
                    "SELECT used FROM budgets WHERE agent=? AND root=?",
                    params![agent, event.root],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(0);
            if used >= binding.limit {
                tx.execute(
                    "UPDATE inbox SET dispatch='budget-exhausted' WHERE id=?",
                    [&eid],
                )?;
                tx.commit()?;
                continue;
            }
            tx.execute(
                "INSERT INTO budgets VALUES(?,?,1) ON CONFLICT(agent,root) DO UPDATE SET used=used+1",
                params![agent, event.root],
            )?;
            tx.execute("UPDATE inbox SET dispatch='running' WHERE id=?", [&eid])?;
            tx.commit()?;
            let from = match &event.agent {
                Some(label) => format!("{} via {label}", env.signer),
                None => env.signer.clone(),
            };
            return Ok(Some(Work {
                event: eid,
                team: team_id,
                sender: env.signer.clone(),
                sender_agent: event.agent.clone(),
                prompt: format!(
                    "Message from team member {from} ({}) to you as {agent}:\n{}",
                    event.actor, event.text
                ),
                agent,
                binding,
            }));
        }
        Ok(None)
    }
    pub fn finish_work(&self, work: &Work, result: Result<Value>) -> Result<()> {
        let result = result.and_then(|reply| {
            let text = field(&reply, "text")?;
            ensure!(text.len() <= MAX_TEXT, "worker reply exceeds message limit");
            Ok(reply)
        });
        match result {
            Ok(reply) => {
                let text = field(&reply, "text")?;
                self.db.execute_batch("BEGIN IMMEDIATE")?;
                let result = (|| -> Result<()> {
                    self.enqueue(
                        &work.team,
                        "message",
                        "agent",
                        text,
                        Some(work.sender.clone()),
                        Some(work.event.clone()),
                        Value::Null,
                        Some(work.agent.clone()),
                        work.sender_agent.clone(),
                    )?;
                    self.db.execute(
                        "UPDATE inbox SET dispatch='replied' WHERE id=?",
                        [&work.event],
                    )?;
                    // Preserve enable/pause changes made while the model was running.
                    if let Some(mut current) = self.binding(&work.agent)?
                        && current.harness == work.binding.harness
                        && current.cwd == work.binding.cwd
                    {
                        current.session = reply["session"].as_str().map(String::from);
                        self.save_binding(&work.agent, Some(&current))?;
                    }
                    self.set("worker_error", "")?;
                    Ok(())
                })();
                match result {
                    Ok(()) => self.db.execute_batch("COMMIT")?,
                    Err(e) => {
                        self.db.execute_batch("ROLLBACK")?;
                        self.db.execute(
                            "UPDATE inbox SET dispatch='failed' WHERE id=?",
                            [&work.event],
                        )?;
                        self.set("worker_error", &e.to_string())?;
                        return Err(e);
                    }
                }
            }
            Err(e) => {
                self.db.execute(
                    "UPDATE inbox SET dispatch='failed' WHERE id=?",
                    [&work.event],
                )?;
                self.set("worker_error", &format!("{e:#}"))?;
            }
        }
        Ok(())
    }
}
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0 as i32), libc::SIGKILL);
        }
    }
}
pub async fn execute(work: &Work) -> Result<Value> {
    ensure!(
        Path::new(&work.binding.adapter).is_file(),
        "worker adapter missing"
    );
    let mut process = tokio::process::Command::new("node")
        .arg(&work.binding.adapter)
        .current_dir(&work.binding.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()?;
    let _group = ProcessGroup(process.id().context("worker process did not start")?);
    let input = json!({"harness":work.binding.harness,"cwd":work.binding.cwd,"session":work.binding.session,"prompt":work.prompt,"timeout_ms":work.binding.timeout_secs*1000,"agent":work.agent});
    let mut stdin = process.stdin.take().context("worker stdin unavailable")?;
    stdin.write_all(&serde_json::to_vec(&input)?).await?;
    drop(stdin);
    let output = tokio::time::timeout(
        Duration::from_secs(work.binding.timeout_secs + 2),
        process.wait_with_output(),
    )
    .await
    .context("worker timed out")??;
    ensure!(
        output.status.success(),
        "worker failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(output.stdout.len() <= 128 * 1024, "worker reply too large");
    Ok(serde_json::from_slice(&output.stdout)?)
}

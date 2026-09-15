use crate::{client::Client, protocol::*, service::field};
use anyhow::{Context, Result, ensure};
use rusqlite::OptionalExtension;
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
}
#[derive(Clone)]
pub struct Work {
    pub event: String,
    pub sender: String,
    pub binding: Binding,
    pub prompt: String,
}
impl Client {
    pub fn worker_command(&self, cmd: &Value) -> Result<Value> {
        let op = field(cmd, "operation")?;
        if op == "bind" {
            let harness = field(cmd, "harness")?;
            ensure!(
                ["codex", "claude"].contains(&harness),
                "unsupported harness"
            );
            let cwd = std::fs::canonicalize(field(cmd, "cwd")?)?;
            ensure!(cwd.is_dir(), "worker cwd is not a directory");
            let adapter = std::fs::canonicalize(field(cmd, "adapter")?)?;
            ensure!(
                adapter.is_file(),
                "worker adapter is missing; build adapters first"
            );
            let b = Binding {
                harness: harness.into(),
                cwd: cwd.to_string_lossy().into(),
                adapter: adapter.to_string_lossy().into(),
                enabled: false,
                limit: 3,
                timeout_secs: 120,
                session: None,
            };
            self.set("worker", &serde_json::to_string(&b)?)?;
        } else if op == "enable" || op == "pause" {
            let mut b: Binding =
                serde_json::from_str(&self.config("worker")?.context("bind a worker first")?)?;
            b.enabled = op == "enable";
            self.set("worker", &serde_json::to_string(&b)?)?;
        } else if op == "reset" {
            let root = field(cmd, "root")?;
            valid_id(root)?;
            self.db
                .execute("DELETE FROM budgets WHERE root=?", [root])?;
            self.db.execute("UPDATE inbox SET dispatch='pending' WHERE dispatch='budget-exhausted' AND json_extract(event,'$.root')=?",[root])?;
        } else {
            ensure!(op == "status", "unknown worker operation");
        }
        Ok(
            json!({"binding":self.config("worker")?.map(|x|serde_json::from_str::<Value>(&x)).transpose()?,"last_error":self.config("worker_error")?}),
        )
    }
    pub fn claim_work(&mut self) -> Result<Option<Work>> {
        let Some(binding) = self.config("worker")? else {
            return Ok(None);
        };
        let binding: Binding = serde_json::from_str(&binding)?;
        if !binding.enabled {
            return Ok(None);
        };
        let team = self.team()?;
        let me = self.identity.member()?.id;
        ensure!(team.members.contains_key(&me), "membership denied");
        let row:Option<(String,String,String)>=self.db.query_row("SELECT id,event,envelope FROM inbox WHERE dispatch='pending' AND json_extract(event,'$.to')=? ORDER BY seq LIMIT 1",[&me],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let Some((eid, text, envelope)) = row else {
            return Ok(None);
        };
        let event: Event = serde_json::from_str(&text)?;
        let env: Signed<Sealed> = serde_json::from_str(&envelope)?;
        if env.signer == me
            || !team.members.contains_key(&env.signer)
            || env.body.header.kind != "message"
        {
            self.db
                .execute("UPDATE inbox SET dispatch='ignored' WHERE id=?", [eid])?;
            return Ok(None);
        }
        let tx = self.db.transaction()?;
        let used: i64 = tx
            .query_row(
                "SELECT used FROM budgets WHERE root=?",
                [&event.root],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0);
        if used >= binding.limit {
            tx.execute(
                "UPDATE inbox SET dispatch='budget-exhausted' WHERE id=?",
                [eid],
            )?;
            tx.commit()?;
            return Ok(None);
        }
        tx.execute(
            "INSERT INTO budgets VALUES(?,1) ON CONFLICT(root) DO UPDATE SET used=used+1",
            [&event.root],
        )?;
        tx.execute("UPDATE inbox SET dispatch='running' WHERE id=?", [&eid])?;
        tx.commit()?;
        Ok(Some(Work {
            event: eid,
            sender: env.signer.clone(),
            binding,
            prompt: format!(
                "Message from team member {} ({}):\n{}",
                env.signer, event.actor, event.text
            ),
        }))
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
                        "message",
                        "agent",
                        text,
                        Some(work.sender.clone()),
                        Some(work.event.clone()),
                        Value::Null,
                    )?;
                    self.db.execute(
                        "UPDATE inbox SET dispatch='replied' WHERE id=?",
                        [&work.event],
                    )?;
                    // Preserve changes to enabled/pause made while the model was running.
                    let mut current: Binding = serde_json::from_str(
                        &self
                            .config("worker")?
                            .context("worker unbound during turn")?,
                    )?;
                    if current.harness == work.binding.harness && current.cwd == work.binding.cwd {
                        current.session = reply["session"].as_str().map(String::from);
                        self.set("worker", &serde_json::to_string(&current)?)?;
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
    let input = json!({"harness":work.binding.harness,"cwd":work.binding.cwd,"session":work.binding.session,"prompt":work.prompt,"timeout_ms":work.binding.timeout_secs*1000});
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

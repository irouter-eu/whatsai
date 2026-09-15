use crate::{client::Client, protocol::*};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;
use tokio::process::Command;

async fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.file.allow=never",
            "-c",
            "protocol.ext.allow=never",
            "-c",
            "submodule.recurse=false",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "Git operation failed; verify local remote credentials and commit availability: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}
impl Client {
    pub async fn accept_handoff(&self, event_id: &str, repo: &Path, dest: &Path) -> Result<Value> {
        let team = self.team()?;
        let body: String =
            self.db
                .query_row("SELECT event FROM inbox WHERE id=?", [event_id], |r| {
                    r.get(0)
                })?;
        let envelope: String =
            self.db
                .query_row("SELECT envelope FROM inbox WHERE id=?", [event_id], |r| {
                    r.get(0)
                })?;
        let envelope: Signed<Sealed> = serde_json::from_str(&envelope)?;
        ensure!(envelope.body.header.kind == "handoff", "not a handoff");
        let event: Event = serde_json::from_str(&body)?;
        ensure!(
            event.data["repository"] == team.id,
            "handoff belongs to another repository"
        );
        let commit = event.data["commit"].as_str().context("missing commit")?;
        ensure!(
            [40, 64].contains(&commit.len()) && hex::decode(commit).is_ok(),
            "invalid commit hash"
        );
        let configured = git(repo, &["remote", "get-url", "origin"]).await?;
        validate_remote(&configured)?;
        ensure!(
            configured == team.repository,
            "local origin does not match the team repository; confirm it locally first"
        );
        ensure!(
            !dest.exists() && !dest.as_os_str().is_empty(),
            "worktree destination must not exist"
        );
        let absolute = if dest.is_absolute() {
            dest.to_path_buf()
        } else {
            std::env::current_dir()?.join(dest)
        };
        git(
            repo,
            &[
                "fetch",
                "--no-tags",
                "--no-recurse-submodules",
                "origin",
                commit,
            ],
        )
        .await?;
        let resolved = git(
            repo,
            &["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
        )
        .await?;
        ensure!(resolved == commit, "resolved commit differs from handoff");
        git(
            repo,
            &[
                "worktree",
                "add",
                "--detach",
                "--",
                absolute.to_str().context("invalid destination")?,
                commit,
            ],
        )
        .await?;
        Ok(json!({"state":"fetched","commit":commit,"worktree":absolute}))
    }
}

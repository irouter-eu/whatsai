use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const VERSION: u32 = 1;
pub const ALPN: &[u8] = b"whatsai/team/1";
pub const MAX_TEXT: usize = 64 * 1024;
pub const CHUNK_SIZE: usize = 1024 * 1024;
pub const MAX_FILE: usize = 32 * CHUNK_SIZE;
pub const RETENTION: i64 = 7 * 24 * 3600;
pub const MAX_FRAME: usize = 4 * CHUNK_SIZE;
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}
pub fn valid_id(s: &str) -> Result<()> {
    ensure!(uuid::Uuid::parse_str(s).is_ok(), "invalid identifier");
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub id: String,
    pub encryption_key: String,
    pub name: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Signed<T> {
    pub body: T,
    pub signer: String,
    pub signature: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Request {
    pub version: u32,
    pub nonce: String,
    pub timestamp: i64,
    pub operation: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
/// The join key: a team, its network secret, the founder, and where the founder's daemon can be reached.
pub struct Invite {
    pub version: u32,
    pub team: String,
    pub founder: String,
    pub secret: String,
    pub authority: Value,
    #[serde(default)]
    pub workspace: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Governance {
    pub team: String,
    pub revision: usize,
    pub previous: String,
    pub action: String,
    pub member: Option<Member>,
    pub target: Option<String>,
    pub repository: Option<String>,
    /// The workspace name a team is bound to; set on create, derived from the repository if absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub founder: String,
    /// The credential-free remote the team works on, when it has one. Git is optional: a team
    /// can be bound to a workspace directory alone.
    pub repository: Option<String>,
    /// The workspace name shown to members and used to match checkouts.
    #[serde(default)]
    pub workspace: String,
    pub revision: usize,
    pub members: BTreeMap<String, Member>,
    pub admins: Vec<String>,
    pub history: Vec<Signed<Governance>>,
    pub presence: BTreeMap<String, i64>,
    pub endpoints: BTreeMap<String, Value>,
    /// Each member's published agents: label, harness, workspace name, repository, online, last_seen.
    #[serde(default)]
    pub agents: BTreeMap<String, Value>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Header {
    pub version: u32,
    pub id: String,
    pub team: String,
    pub sender: String,
    pub created: i64,
    pub kind: String,
    pub recipients: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WrappedKey {
    pub encapsulated: String,
    pub ciphertext: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sealed {
    pub header: Header,
    pub nonce: String,
    pub ciphertext: String,
    pub keys: BTreeMap<String, WrappedKey>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub actor: String,
    pub text: String,
    pub to: Option<String>,
    pub reply_to: Option<String>,
    pub root: String,
    pub data: Value,
    /// The sender's agent label (`harness@workspace`), when an agent rather than the person wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// One of the recipient's agents; unset means the person and every agent of theirs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_agent: Option<String>,
}
impl Event {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.actor == "person" || self.actor == "agent",
            "invalid actor"
        );
        ensure!(self.text.len() <= MAX_TEXT, "message exceeds 64 KiB");
        valid_id(&self.root)?;
        if let Some(x) = &self.reply_to {
            valid_id(x)?;
        }
        if let Some(label) = &self.agent {
            valid_label(label)?;
        }
        if let Some(label) = &self.to_agent {
            valid_label(label)?;
            ensure!(self.to.is_some(), "an agent address needs a member address");
        }
        Ok(())
    }
}
/// A workspace name is what a team is called and matched by; it must print cleanly anywhere.
pub fn validate_workspace_name(name: &str) -> Result<()> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 64 && !name.chars().any(char::is_control),
        "invalid workspace name"
    );
    Ok(())
}
/// The workspace name for a directory: its final component.
pub fn workspace_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "workspace".into())
}
/// Agent labels are `harness@workspace`, short and safe to print anywhere.
pub fn valid_label(label: &str) -> Result<()> {
    let (harness, workspace) = label
        .split_once('@')
        .context("agent label must be harness@workspace")?;
    valid_harness(harness)?;
    ensure!(
        !workspace.is_empty()
            && workspace.len() <= 48
            && workspace
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')),
        "invalid workspace name in agent label"
    );
    Ok(())
}
pub fn valid_harness(harness: &str) -> Result<()> {
    ensure!(
        !harness.is_empty()
            && harness.len() <= 32
            && harness
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_')),
        "invalid harness name {harness:?}: use lowercase letters, digits, '-' or '_'"
    );
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub name: String,
    pub size: usize,
    pub sha256: String,
    pub chunks: Vec<String>,
}
impl Manifest {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            !self.name.is_empty()
                && self.name != "."
                && self.name != ".."
                && !self.name.contains(['/', '\\', '\0'])
                && self.name.len() <= 255,
            "unsafe filename"
        );
        ensure!(self.size <= MAX_FILE, "file exceeds 32 MiB");
        ensure!(
            self.chunks.len() == self.size.div_ceil(CHUNK_SIZE),
            "invalid chunk count"
        );
        ensure!(
            self.sha256.len() == 64 && hex::decode(&self.sha256).is_ok(),
            "invalid file hash"
        );
        for h in &self.chunks {
            ensure!(
                h.len() == 64 && hex::decode(h).is_ok(),
                "invalid chunk hash"
            );
        }
        Ok(())
    }
}
/// Only share credential-free network remotes; never local paths or URL tokens.
pub fn validate_remote(remote: &str) -> Result<()> {
    ensure!(
        !remote.contains(['\n', '\r', '\0', ' ']),
        "invalid repository remote"
    );
    if let Ok(url) = url::Url::parse(remote) {
        ensure!(
            ["https", "ssh"].contains(&url.scheme()) && url.host_str().is_some(),
            "use an HTTPS or SSH Git remote"
        );
        ensure!(
            url.password().is_none() && url.query().is_none() && url.fragment().is_none(),
            "repository remote contains credentials or query data"
        );
        ensure!(
            url.username().is_empty() || (url.scheme() == "ssh" && url.username() == "git"),
            "repository remote must not contain credentials"
        );
    } else {
        let rest = remote
            .strip_prefix("git@")
            .ok_or_else(|| anyhow::anyhow!("use a credential-free network Git remote"))?;
        let (host, path) = rest
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("invalid SSH remote"))?;
        ensure!(
            !host.is_empty() && !host.contains('/') && !path.is_empty(),
            "invalid SSH remote"
        );
    }
    Ok(())
}
/// A network secret is 32 random bytes; possession lets you ask to join, never more.
pub fn validate_secret(secret: &str) -> Result<()> {
    ensure!(
        secret.len() == 64 && hex::decode(secret).is_ok(),
        "invalid network secret"
    );
    Ok(())
}
pub fn secret_matches(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

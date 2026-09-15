use anyhow::{Result, ensure};
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
pub struct Invite {
    pub version: u32,
    pub service: String,
    pub team: String,
    pub founder: String,
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
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub founder: String,
    pub repository: String,
    pub revision: usize,
    pub members: BTreeMap<String, Member>,
    pub admins: Vec<String>,
    pub history: Vec<Signed<Governance>>,
    pub presence: BTreeMap<String, i64>,
    pub endpoints: BTreeMap<String, Value>,
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
        Ok(())
    }
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
pub fn validate_service(service: &str) -> Result<()> {
    let u = url::Url::parse(service)?;
    ensure!(
        u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none(),
        "invalid service URL"
    );
    ensure!(
        u.scheme() == "https"
            || (u.scheme() == "http"
                && matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))),
        "service requires HTTPS (HTTP is allowed only on loopback)"
    );
    Ok(())
}

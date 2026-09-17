use crate::crypto::Identity;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use rusqlite::Connection;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

pub fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.is_dir() && !meta.file_type().is_symlink(),
        "state directory must be a real directory"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
pub fn write_private(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("{}.tmp", crate::protocol::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(data)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}
pub fn lock(dir: &Path) -> Result<File> {
    private_dir(dir)?;
    let f = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(dir.join("runtime.lock"))?;
    f.try_lock_exclusive()
        .context("another process owns this state directory")?;
    Ok(f)
}
pub fn identity(dir: &Path, name: &str) -> Result<Identity> {
    private_dir(dir)?;
    let p = dir.join("identity.json");
    if p.exists() {
        let i: Identity = serde_json::from_slice(&fs::read(&p)?)?;
        i.member()?;
        return Ok(i);
    }
    ensure!(
        !dir.join("client.db").exists(),
        "identity missing beside existing state; restore keys rather than silently replacing them"
    );
    let i = Identity::generate(name);
    write_private(&p, &serde_json::to_vec_pretty(&i)?)?;
    Ok(i)
}
pub fn database(path: &Path, ddl: &str) -> Result<Connection> {
    if !path.exists() {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
    }
    let c = Connection::open(path)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    let version: i64 = c.pragma_query_value(None, "user_version", |r| r.get(0))?;
    ensure!(
        version <= 1,
        "unsupported database version {version}; restore a compatible backup"
    );
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    if version == 0 {
        c.execute_batch(&format!(
            "BEGIN IMMEDIATE;{ddl} PRAGMA user_version=1;COMMIT;"
        ))?;
    }
    Ok(c)
}
/// The state directory for a harness: `WHATSAI_STATE` wins; otherwise each harness (and the
/// plain CLI) gets its own directory under the base, so one machine can hold one identity per
/// coding agent and they join a team as distinct members.
pub fn default_state_for(harness: Option<&str>) -> Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("WHATSAI_STATE") {
        return Ok(PathBuf::from(explicit));
    }
    let harness = harness
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .unwrap_or("cli");
    ensure!(
        harness.len() <= 32
            && harness
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "invalid harness name {harness:?}: use letters, digits, '-' or '_'"
    );
    Ok(base_state().join(harness))
}
pub fn base_state() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
        .join(".local/share/whatsai")
}
pub fn default_state() -> Result<PathBuf> {
    let harness = std::env::var("WHATSAI_HARNESS").ok();
    default_state_for(harness.as_deref())
}
pub const CLIENT_SCHEMA: &str = r#"
CREATE TABLE config(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE outbox(id TEXT PRIMARY KEY,envelope TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'queued',error TEXT);
CREATE TABLE inbox(id TEXT PRIMARY KEY,seq INTEGER NOT NULL,event TEXT NOT NULL,envelope TEXT NOT NULL,dispatch TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE chunks(file TEXT NOT NULL,idx INTEGER NOT NULL,envelope TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'queued',PRIMARY KEY(file,idx));
CREATE TABLE downloads(file TEXT NOT NULL,idx INTEGER NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(file,idx));
CREATE TABLE budgets(root TEXT PRIMARY KEY,used INTEGER NOT NULL);
CREATE TABLE invites(hash TEXT PRIMARY KEY,expires INTEGER NOT NULL);
"#;

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
    // A daemon that was just stopped releases its lock a moment after its socket disappears;
    // give it that moment instead of failing a restart that raced it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        match f.try_lock_exclusive() {
            Ok(()) => return Ok(f),
            Err(e) if std::time::Instant::now() < deadline => {
                let _ = e;
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(e) => return Err(e).context("another process owns this state directory"),
        }
    }
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
/// Open a database and bring it to the newest schema. `migrations[i]` takes user_version i to i+1.
pub fn database(path: &Path, migrations: &[&str]) -> Result<Connection> {
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
        version as usize <= migrations.len(),
        "unsupported database version {version}; restore a compatible backup"
    );
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON;")?;
    for (index, ddl) in migrations.iter().enumerate().skip(version as usize) {
        let next = index + 1;
        c.execute_batch(&format!(
            "BEGIN IMMEDIATE;{ddl} PRAGMA user_version={next};COMMIT;"
        ))?;
    }
    Ok(c)
}
/// One member per person per machine: `WHATSAI_STATE` or `~/.local/share/whatsai`. Agents and
/// sessions live inside that one identity rather than getting directories of their own.
pub fn default_state() -> PathBuf {
    std::env::var_os("WHATSAI_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(base_state)
}
pub fn base_state() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_else(|| ".".into()))
        .join(".local/share/whatsai")
}
pub const CLIENT_MIGRATIONS: &[&str] = &[CLIENT_SCHEMA_V1, CLIENT_SCHEMA_V2, CLIENT_SCHEMA_V3];
const CLIENT_SCHEMA_V1: &str = r#"
CREATE TABLE config(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE outbox(id TEXT PRIMARY KEY,envelope TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'queued',error TEXT);
CREATE TABLE inbox(id TEXT PRIMARY KEY,seq INTEGER NOT NULL,event TEXT NOT NULL,envelope TEXT NOT NULL,dispatch TEXT NOT NULL DEFAULT 'pending');
CREATE TABLE chunks(file TEXT NOT NULL,idx INTEGER NOT NULL,envelope TEXT NOT NULL,state TEXT NOT NULL DEFAULT 'queued',PRIMARY KEY(file,idx));
CREATE TABLE downloads(file TEXT NOT NULL,idx INTEGER NOT NULL,bytes BLOB NOT NULL,PRIMARY KEY(file,idx));
CREATE TABLE budgets(root TEXT PRIMARY KEY,used INTEGER NOT NULL);
CREATE TABLE invites(hash TEXT PRIMARY KEY,expires INTEGER NOT NULL);
"#;
/// Durable agents keyed by harness and workspace, ephemeral sessions attached to them, and
/// per-agent worker bindings, read cursors and reply budgets.
const CLIENT_SCHEMA_V2: &str = r#"
CREATE TABLE agents(label TEXT PRIMARY KEY,harness TEXT NOT NULL,workspace TEXT NOT NULL,repository TEXT,created INTEGER NOT NULL,last_seen INTEGER NOT NULL,retired INTEGER NOT NULL DEFAULT 0,worker TEXT,cursor INTEGER NOT NULL DEFAULT 0,UNIQUE(harness,workspace));
CREATE TABLE sessions(lease TEXT PRIMARY KEY,agent TEXT NOT NULL REFERENCES agents(label) ON DELETE CASCADE,session TEXT,pid INTEGER,started INTEGER NOT NULL,heartbeat INTEGER NOT NULL);
DROP TABLE budgets;
CREATE TABLE budgets(agent TEXT NOT NULL,root TEXT NOT NULL,used INTEGER NOT NULL,PRIMARY KEY(agent,root));
DELETE FROM config WHERE key='worker';
"#;
/// Nothing about an agent leaves the machine until it is published on purpose.
const CLIENT_SCHEMA_V3: &str = r#"
ALTER TABLE agents ADD COLUMN published INTEGER NOT NULL DEFAULT 0;
"#;

use serde_json::json;
use std::{os::unix::fs::PermissionsExt, path::Path, process::Command};
use whatsai_core::{client::Client, governance::replay, protocol::*};
fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8(o.stdout).unwrap().trim().into()
}
#[tokio::test]
async fn exact_commit_handoff_preserves_dirty_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let bare = tmp.path().join("bare.git");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "test@example.invalid"]);
    git(&repo, &["config", "user.name", "Synthetic Test"]);
    std::fs::write(repo.join("file.txt"), "committed\n").unwrap();
    git(&repo, &["add", "file.txt"]);
    git(&repo, &["commit", "-qm", "fixture"]);
    let commit = git(&repo, &["rev-parse", "HEAD"]);
    git(&repo, &["clone", "--bare", ".", bare.to_str().unwrap()]);
    let ssh = tmp.path().join("fake-ssh");
    std::fs::write(
        &ssh,
        format!("#!/bin/sh\nexec git-upload-pack '{}'\n", bare.display()),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &repo,
        &["remote", "add", "origin", "git@localhost:fixture.git"],
    );
    git(&repo, &["config", "core.sshCommand", ssh.to_str().unwrap()]);
    git(&repo, &["config", "ssh.variant", "simple"]);
    std::fs::write(repo.join("file.txt"), "uncommitted local work\n").unwrap();
    let mut c = Client::open(&tmp.path().join("client"), "Alice").unwrap();
    let tid = id();
    let member = c.identity.member().unwrap();
    let record = c
        .identity
        .sign(Governance {
            team: tid.clone(),
            revision: 0,
            previous: String::new(),
            action: "create".into(),
            member: Some(member.clone()),
            target: None,
            repository: Some("git@localhost:fixture.git".into()),
        })
        .unwrap();
    let team = replay(&[record], &member.id).unwrap();
    c.set("team", &serde_json::to_string(&team).unwrap())
        .unwrap();
    let eid = id();
    let ev = Event {
        actor: "agent".into(),
        text: "Review this commit".into(),
        to: None,
        reply_to: None,
        root: eid.clone(),
        data: json!({"repository":tid,"branch":"main","commit":commit}),
    };
    let h = Header {
        version: VERSION,
        id: eid.clone(),
        team: tid,
        sender: member.id.clone(),
        created: now(),
        kind: "handoff".into(),
        recipients: vec![member.id.clone()],
    };
    let env = c
        .identity
        .seal(h, &serde_json::to_vec(&ev).unwrap(), &[member])
        .unwrap();
    c.receive(1, &env).unwrap();
    let dest = tmp.path().join("review");
    c.accept_handoff(&eid, &repo, &dest).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(repo.join("file.txt")).unwrap(),
        "uncommitted local work\n"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("file.txt")).unwrap(),
        "committed\n"
    );
    assert_eq!(git(&dest, &["rev-parse", "HEAD"]), commit);
    assert!(c.accept_handoff(&eid, &repo, &dest).await.is_err());
    git(
        &repo,
        &["remote", "set-url", "origin", "git@localhost:other.git"],
    );
    assert!(
        c.accept_handoff(&eid, &repo, &tmp.path().join("other"))
            .await
            .is_err()
    );
}

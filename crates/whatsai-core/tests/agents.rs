use serde_json::{Value, json};
use tempfile::TempDir;
use whatsai_core::{agents::SESSION_TTL, client::Client, crypto::*, governance::*, protocol::*};

thread_local! {
    /// The team id the current test installed, for presence checks.
    static TEAM: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}
fn fixture_authority() -> iroh::EndpointAddr {
    iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[9u8; 32]).public())
}

fn team_with(receiver: &Client) -> (Identity, Team) {
    let a = Identity::generate("Sender");
    let tid = id();
    let create = a
        .sign(Governance {
            team: tid.clone(),
            revision: 0,
            previous: String::new(),
            action: "create".into(),
            member: Some(a.member().unwrap()),
            target: None,
            repository: Some("https://example.com/team/repo.git".into()),
            workspace: None,
        })
        .unwrap();
    let t = replay(std::slice::from_ref(&create), &a.member().unwrap().id).unwrap();
    let admit = a
        .sign(Governance {
            team: tid.clone(),
            revision: 1,
            previous: digest(&serde_json::to_vec(&create).unwrap()),
            action: "admit".into(),
            member: Some(receiver.identity.member().unwrap()),
            target: None,
            repository: None,
            workspace: None,
        })
        .unwrap();
    let t = replay(&[create, admit], &t.founder).unwrap();
    receiver
        .install_team(&t, &fixture_authority(), &"7".repeat(64), None)
        .unwrap();
    TEAM.with(|id| *id.borrow_mut() = t.id.clone());
    (a, t)
}
fn deliver(
    c: &mut Client,
    from: &Identity,
    t: &Team,
    seq: i64,
    to_agent: Option<&str>,
    agent: Option<&str>,
) {
    let me = c.identity.member().unwrap().id;
    let e = Event {
        actor: "agent".into(),
        text: format!("message {seq}"),
        to: Some(me),
        reply_to: None,
        root: id(),
        data: Value::Null,
        agent: agent.map(String::from),
        to_agent: to_agent.map(String::from),
    };
    let h = Header {
        version: VERSION,
        id: id(),
        team: t.id.clone(),
        sender: from.member().unwrap().id,
        created: now(),
        kind: "message".into(),
        recipients: t.members.keys().cloned().collect(),
    };
    let env = from
        .seal(
            h,
            &serde_json::to_vec(&e).unwrap(),
            &t.members.values().cloned().collect::<Vec<_>>(),
        )
        .unwrap();
    c.receive(seq, &env).unwrap();
}

#[test]
fn agents_are_durable_and_sessions_are_not() {
    let tmp = TempDir::new().unwrap();
    let c = Client::open(tmp.path(), "Person").unwrap();
    team_with(&c);
    let repo = tmp.path().join("whatsai");
    std::fs::create_dir(&repo).unwrap();
    let first = c
        .attach("claude", &repo, Some("s1"), Some(100), None)
        .unwrap();
    assert_eq!(first["agent"]["label"], "claude@whatsai");
    assert_eq!(first["agent"]["sessions"], 1);
    let second = c
        .attach("claude", &repo, Some("s2"), Some(101), None)
        .unwrap();
    assert_eq!(
        second["agent"]["label"], "claude@whatsai",
        "same workspace, same agent"
    );
    assert_eq!(second["agent"]["sessions"], 2);
    let codex = c.attach("codex", &repo, None, None, None).unwrap();
    assert_eq!(
        codex["agent"]["label"], "codex@whatsai",
        "another harness is another agent"
    );
    c.detach(first["lease"].as_str().unwrap()).unwrap();
    assert_eq!(c.agent("claude@whatsai").unwrap()["sessions"], 1);
    // A silent session expires; the agent stays, offline.
    c.db.execute("UPDATE sessions SET heartbeat=?", [now() - SESSION_TTL - 1])
        .unwrap();
    let agent = c.agent("claude@whatsai").unwrap();
    assert_eq!(agent["sessions"], 0);
    assert_eq!(agent["online"], false);
    assert_eq!(agent["retired"], false);
    assert!(
        c.heartbeat(second["lease"].as_str().unwrap()).is_err(),
        "expired leases are gone"
    );
    // Reattaching from the same checkout is the same agent again.
    let again = c.attach("claude", &repo, Some("s3"), None, None).unwrap();
    assert_eq!(again["agent"]["label"], "claude@whatsai");
    assert!(c.heartbeat(again["lease"].as_str().unwrap()).is_ok());
}

#[test]
fn labels_disambiguate_and_adopt_moves_the_work() {
    let tmp = TempDir::new().unwrap();
    let c = Client::open(tmp.path(), "Person").unwrap();
    team_with(&c);
    let a = tmp.path().join("one/app");
    let b = tmp.path().join("two/app");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    assert_eq!(
        c.attach("claude", &a, None, None, None).unwrap()["agent"]["label"],
        "claude@app"
    );
    assert_eq!(
        c.attach("claude", &b, None, None, None).unwrap()["agent"]["label"],
        "claude@app-2"
    );
    assert!(
        c.attach("Claude Code", &a, None, None, None).is_err(),
        "harness names are validated"
    );
    let moved = tmp.path().join("three/app");
    std::fs::create_dir_all(&moved).unwrap();
    // The destination already has a placeholder agent nobody was told about.
    assert_eq!(
        c.attach("claude", &moved, None, None, None).unwrap()["agent"]["label"],
        "claude@app-3"
    );
    let adopted = c.adopt("claude@app", &moved).unwrap();
    assert_eq!(
        adopted["workspace"].as_str().unwrap(),
        std::fs::canonicalize(&moved).unwrap().to_str().unwrap()
    );
    let labels: Vec<String> = c
        .agents()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["label"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        labels,
        vec!["claude@app", "claude@app-2"],
        "placeholder removed, label kept"
    );
    assert_eq!(
        c.resolve_agent("claude", &moved).unwrap().as_deref(),
        Some("claude@app")
    );
    assert_eq!(c.resolve_agent("claude", &a).unwrap(), None);
    assert_eq!(
        c.agent_presence(&TEAM.with(|t| t.borrow().clone()))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0,
        "attaching publishes nothing"
    );
    c.publish("claude@app", true).unwrap();
    c.publish("claude@app-2", true).unwrap();
    c.retire("claude@app-2").unwrap();
    assert_eq!(
        c.resolve_agent("claude", &b).unwrap(),
        None,
        "retired agents are not offered"
    );
    let presence = c
        .agent_presence(&TEAM.with(|t| t.borrow().clone()))
        .unwrap();
    assert_eq!(
        presence.as_array().unwrap().len(),
        1,
        "retired agents are not published"
    );
    assert_eq!(
        presence[0]["workspace"], "app",
        "only the workspace name leaves the machine"
    );
    c.publish("claude@app", false).unwrap();
    assert_eq!(
        c.agent_presence(&TEAM.with(|t| t.borrow().clone()))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0,
        "unpublish hides it again"
    );
    assert!(
        c.publish("claude@app-2", true).is_err(),
        "retired agents cannot be published"
    );
}

#[test]
fn unread_counts_follow_addressing_and_cursors() {
    let tmp = TempDir::new().unwrap();
    let mut c = Client::open(tmp.path(), "Person").unwrap();
    let (sender, t) = team_with(&c);
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let claude = c.attach("claude", &repo, None, None, None).unwrap()["agent"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    let codex = c.attach("codex", &repo, None, None, None).unwrap()["agent"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    c.enroll(&claude, true, None).unwrap();
    c.enroll(&codex, true, None).unwrap();
    deliver(&mut c, &sender, &t, 1, Some(&claude), Some("claude@theirs"));
    deliver(&mut c, &sender, &t, 2, Some(&codex), None);
    deliver(&mut c, &sender, &t, 3, None, None);
    let claude_unread = c.unread(&claude).unwrap();
    assert_eq!(
        (
            claude_unread["addressed"].as_i64(),
            claude_unread["shared"].as_i64()
        ),
        (Some(1), Some(1))
    );
    let codex_unread = c.unread(&codex).unwrap();
    assert_eq!(
        (
            codex_unread["addressed"].as_i64(),
            codex_unread["shared"].as_i64()
        ),
        (Some(1), Some(1))
    );
    let visible = c.inbox_for(None, Some(&claude), false).unwrap();
    assert_eq!(
        visible.as_array().unwrap().len(),
        2,
        "claude sees its own and the shared message, not codex's"
    );
    assert_eq!(visible[0]["event"]["agent"], "claude@theirs");
    let after = c.mark_read(&claude).unwrap();
    assert_eq!(
        (after["addressed"].as_i64(), after["shared"].as_i64()),
        (Some(0), Some(0))
    );
    assert_eq!(
        c.inbox_for(None, Some(&claude), true)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        c.unread(&codex).unwrap()["addressed"],
        1,
        "cursors are per agent"
    );
    let all = c.inbox().unwrap();
    assert_eq!(all.as_array().unwrap().len(), 3);
    assert!(c.unread("claude@nowhere").is_err());
}

#[test]
fn workers_answer_only_their_agent_and_budgets_are_per_agent() {
    let tmp = TempDir::new().unwrap();
    let mut c = Client::open(tmp.path(), "Person").unwrap();
    let (sender, t) = team_with(&c);
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let adapter = tmp.path().join("adapter.js");
    std::fs::write(&adapter, "").unwrap();
    let claude = c.attach("claude", &repo, None, None, None).unwrap()["agent"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    let codex = c.attach("codex", &repo, None, None, None).unwrap()["agent"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    c.enroll(&claude, true, None).unwrap();
    c.enroll(&codex, true, None).unwrap();
    c.worker_command(&json!({"operation":"bind","agent":claude,"adapter":adapter}))
        .unwrap();
    c.worker_command(&json!({"operation":"bind","agent":codex,"adapter":adapter,"limit":1}))
        .unwrap();
    c.worker_command(&json!({"operation":"enable","agent":claude}))
        .unwrap();
    c.worker_command(&json!({"operation":"enable","agent":codex}))
        .unwrap();
    deliver(&mut c, &sender, &t, 1, None, None); // nobody is default: stays in the inbox
    deliver(&mut c, &sender, &t, 2, Some(&codex), None);
    deliver(&mut c, &sender, &t, 3, Some(&claude), Some("codex@theirs"));
    let first = c.claim_work().unwrap().unwrap();
    assert_eq!(first.agent, claude, "agents are served in label order");
    assert!(first.prompt.contains("via codex@theirs") && first.prompt.contains(&claude));
    c.finish_work(&first, Ok(json!({"text":"done","session":"sess-1"})))
        .unwrap();
    let reply: String =
        c.db.query_row(
            "SELECT envelope FROM outbox ORDER BY rowid DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let reply: Signed<Sealed> = serde_json::from_str(&reply).unwrap();
    let reply: Event = serde_json::from_slice(&sender.open(&reply).unwrap()).unwrap();
    assert_eq!(
        (reply.agent.as_deref(), reply.to_agent.as_deref()),
        (Some(claude.as_str()), Some("codex@theirs")),
        "replies go back to the asking agent"
    );
    let second = c.claim_work().unwrap().unwrap();
    assert_eq!(second.agent, codex);
    c.finish_work(&second, Ok(json!({"text":"ok"}))).unwrap();
    assert!(
        c.claim_work().unwrap().is_none(),
        "the unaddressed message is left for people"
    );
    let status = c.worker_command(&json!({"operation":"status"})).unwrap();
    assert_eq!(status["workers"].as_array().unwrap().len(), 2);
    assert_eq!(c.agent(&claude).unwrap()["worker"]["session"], "sess-1");
    let pending: i64 =
        c.db.query_row(
            "SELECT count(*) FROM inbox WHERE dispatch='pending'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending, 1);
    c.retire(&codex).unwrap();
    assert!(
        c.worker_command(&json!({"operation":"enable","agent":codex}))
            .is_err(),
        "retired agents drop their worker"
    );
}

#[test]
fn version_one_databases_upgrade_in_place() {
    use whatsai_core::storage;
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("client.db");
    storage::identity(tmp.path(), "Upgraded").unwrap();
    {
        let old = storage::database(&path, &storage::CLIENT_MIGRATIONS[..1]).unwrap();
        old.execute(
            "INSERT INTO config VALUES('worker','{\"legacy\":true}')",
            [],
        )
        .unwrap();
        old.execute("INSERT INTO budgets VALUES('root-1',2)", [])
            .unwrap();
        let v: i64 = old
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, 1);
    }
    let c = Client::open(tmp.path(), "Upgraded").unwrap();
    let v: i64 =
        c.db.pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
    assert_eq!(v, 5);
    assert!(
        c.config("worker").unwrap().is_none(),
        "single-worker binding is retired"
    );
    assert_eq!(c.agents().unwrap().as_array().unwrap().len(), 0);
    assert!(
        storage::database(&path, &storage::CLIENT_MIGRATIONS[..1]).is_err(),
        "older binaries refuse the newer schema"
    );
}

#[test]
fn publishing_is_explicit_unless_the_owner_opts_in_for_the_team_repository() {
    let tmp = TempDir::new().unwrap();
    let c = Client::open(tmp.path(), "Person").unwrap();
    let (_, team) = team_with(&c);
    let ours = tmp.path().join("ours");
    let theirs = tmp.path().join("theirs");
    for (dir, remote) in [
        (&ours, team.repository.as_deref().unwrap()),
        (&theirs, "https://example.com/other/repo.git"),
    ] {
        std::fs::create_dir(dir).unwrap();
        for args in [vec!["init", "-q"], vec!["remote", "add", "origin", remote]] {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(&args)
                .output()
                .unwrap()
                .status;
            assert!(status.success());
        }
    }
    let a = c.attach("claude", &ours, None, None, None).unwrap()["agent"].clone();
    assert_eq!(
        a["repository"],
        json!(team.repository),
        "the checkout's origin is detected"
    );
    assert_eq!(
        a["published"], false,
        "matching the team repository is not enough by default"
    );
    c.agent_command(&json!({"operation":"auto-publish","mode":"team-repo"}))
        .unwrap();
    assert!(
        c.agent_command(&json!({"operation":"auto-publish","mode":"always"}))
            .is_err()
    );
    let b = c.attach("codex", &ours, None, None, None).unwrap()["agent"].clone();
    assert_eq!(
        b["published"], true,
        "opted in: the team repository publishes on attach"
    );
    let other = c.attach("claude", &theirs, None, None, None).unwrap()["agent"].clone();
    assert_eq!(
        other["published"], false,
        "other repositories never publish themselves"
    );
    let presence = c
        .agent_presence(&TEAM.with(|t| t.borrow().clone()))
        .unwrap();
    assert_eq!(presence.as_array().unwrap().len(), 1);
    assert_eq!(presence[0]["label"], "codex@ours");
    assert_eq!(
        c.attach("claude", &ours, None, None, None).unwrap()["agent"]["published"],
        true,
        "re-attaching under the opt-in publishes the first agent too"
    );
}

#[tokio::test]
async fn sessions_touch_the_team_only_through_enrolled_agents() {
    let tmp = TempDir::new().unwrap();
    let mut c = Client::open(tmp.path(), "Person").unwrap();
    let (sender, t) = team_with(&c);
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let label = c.attach("claude", &elsewhere, None, None, None).unwrap()["agent"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        c.agent(&label).unwrap()["enrolled"],
        false,
        "an unrelated checkout is not part of the team"
    );
    deliver(&mut c, &sender, &t, 1, None, None);
    // Owner commands from the shell carry no `via` and always work.
    assert_eq!(
        c.command(json!({"action":"inbox"}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // A session of an unenrolled agent is refused every team action, sees no inbox and no unread.
    for action in [
        "list", "inbox", "invite", "send", "sync", "requests", "worker",
    ] {
        let err = c
            .command(json!({"action":action,"via":label,"text":"x","operation":"status"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not enrolled"), "{action}: {err}");
    }
    assert!(
        c.command(json!({"action":"health","via":label}))
            .await
            .is_ok(),
        "local actions stay available"
    );
    assert_eq!(
        c.inbox_for(None, Some(&label), false)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let unread = c.unread(&label).unwrap();
    assert_eq!(
        (unread["enrolled"].as_bool(), unread["shared"].as_i64()),
        (Some(false), Some(0))
    );
    let err = c
        .command(json!({"action":"inbox","via":"unattached"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("not enrolled"),
        "an unattached session is refused too: {err}"
    );
    // Enrolling opens the team to that agent's sessions; unenrolling closes it and unpublishes.
    c.enroll(&label, true, None).unwrap();
    assert_eq!(
        c.command(json!({"action":"inbox","via":label}))
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(c.unread(&label).unwrap()["shared"], 1);
    c.publish(&label, true).unwrap();
    assert_eq!(
        c.agent_presence(&TEAM.with(|t| t.borrow().clone()))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let after = c.enroll(&label, false, None).unwrap();
    assert_eq!(
        (after["enrolled"].as_bool(), after["published"].as_bool()),
        (Some(false), Some(false))
    );
    assert_eq!(
        c.agent_presence(&TEAM.with(|t| t.borrow().clone()))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        0
    );
    // Publishing an unenrolled agent enrolls it, since publishing means taking part.
    let published = c.publish(&label, true).unwrap();
    assert_eq!(
        (
            published["enrolled"].as_bool(),
            published["published"].as_bool()
        ),
        (Some(true), Some(true))
    );
    // Checkouts of the team repository enroll themselves unless the owner turns that off.
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", t.repository.as_deref().unwrap()],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(&args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    assert_eq!(
        c.attach("codex", &repo, None, None, None).unwrap()["agent"]["enrolled"],
        true
    );
    c.agent_command(&json!({"operation":"auto-enroll","mode":"off"}))
        .unwrap();
    let again = tmp.path().join("repo2");
    std::fs::create_dir(&again).unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["remote", "add", "origin", t.repository.as_deref().unwrap()],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&again)
                .args(&args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    assert_eq!(
        c.attach("codex", &again, None, None, None).unwrap()["agent"]["enrolled"],
        false
    );
}

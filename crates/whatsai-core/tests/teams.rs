use serde_json::json;
use std::{path::Path, sync::Arc};
use tempfile::TempDir;
use whatsai_core::{client::Client, service::Service, storage};

/// A member that can found teams without a network: the authority runs in-process.
fn founder(tmp: &TempDir) -> Client {
    let mut c = Client::open(&tmp.path().join("state"), "Founder").unwrap();
    c.authority = Some(Arc::new(
        Service::open(&tmp.path().join("authority"), 50_000_000).unwrap(),
    ));
    c
}
fn git(dir: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
        .status;
    assert!(status.success());
}

#[tokio::test]
async fn teams_bind_to_a_directory_or_a_repository_and_git_is_optional() {
    let tmp = TempDir::new().unwrap();
    let mut c = founder(&tmp);
    let notes = tmp.path().join("notes");
    std::fs::create_dir(&notes).unwrap();
    let key = c.create(None, &notes).await.unwrap();
    assert_eq!(key["workspace"], "notes");
    assert_eq!(key["repository"], serde_json::Value::Null);
    assert!(key["join"].as_str().unwrap().starts_with("whatsai1."));
    let repo = tmp.path().join("app");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://example.com/team/app.git",
        ],
    );
    let key2 = c
        .create(Some("https://example.com/team/app.git"), &repo)
        .await
        .unwrap();
    assert_eq!(key2["workspace"], "app");
    assert_eq!(key2["repository"], "https://example.com/team/app.git");
    assert_ne!(key["team"], key2["team"]);
    let teams = c.teams().unwrap();
    assert_eq!(teams.as_array().unwrap().len(), 2);
    assert!(
        teams
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["role"] == "admin")
    );
    assert!(
        c.create(None, &notes).await.is_err(),
        "a workspace belongs to one team"
    );
    // Resolution: by directory, by name, by repository, by id; ambiguity is an error.
    let notes_id = key["team"].as_str().unwrap();
    let app_id = key2["team"].as_str().unwrap();
    assert_eq!(
        c.match_workspace(&notes).unwrap().as_deref(),
        Some(notes_id)
    );
    assert_eq!(c.match_workspace(&repo).unwrap().as_deref(), Some(app_id));
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    assert_eq!(c.match_workspace(&elsewhere).unwrap(), None);
    let same_name = tmp.path().join("other/notes");
    std::fs::create_dir_all(&same_name).unwrap();
    assert_eq!(
        c.match_workspace(&same_name).unwrap(),
        None,
        "a directory name alone never matches"
    );
    assert_eq!(c.find_team("notes").unwrap(), notes_id);
    assert_eq!(
        c.find_team("https://example.com/team/app.git").unwrap(),
        app_id
    );
    assert_eq!(c.find_team(app_id).unwrap(), app_id);
    assert!(c.find_team("nowhere").is_err());
    assert_eq!(c.resolve_team(&json!({"cwd":repo})).unwrap(), app_id);
    assert_eq!(
        c.resolve_team(&json!({"team":"notes","cwd":repo})).unwrap(),
        notes_id,
        "an explicit team wins"
    );
    let err = c
        .resolve_team(&json!({"cwd":elsewhere}))
        .unwrap_err()
        .to_string();
    assert!(err.contains("--team"), "{err}");
    // Agents attach into the matching team and act only there.
    let a = c.attach("claude", &notes, None, None, None).unwrap()["agent"].clone();
    assert_eq!(
        (a["enrolled"].as_bool(), a["team"].as_str()),
        (Some(true), Some(notes_id))
    );
    assert_eq!(
        a["published"], false,
        "a silent attach stays private even in a matching directory"
    );
    assert_eq!(a["team_name"], "notes");
    let b = c.attach("claude", &repo, None, None, None).unwrap()["agent"].clone();
    assert_eq!(b["team"].as_str(), Some(app_id));
    let stray = c.attach("claude", &elsewhere, None, None, None).unwrap()["agent"].clone();
    assert_eq!(stray["enrolled"], false);
    assert_eq!(
        c.resolve_team(&json!({"via":a["label"]})).unwrap(),
        notes_id,
        "a session resolves to its agent's team"
    );
    // Status events land in their own team only.
    c.command(
        json!({"action":"status","state":"working","description":"notes work","team":"notes"}),
    )
    .await
    .unwrap();
    c.command(json!({"action":"status","state":"blocked","description":"app work","cwd":repo}))
        .await
        .unwrap();
    let outbox = c.outbox(Some(app_id)).await.unwrap();
    assert_eq!(outbox.as_array().unwrap().len(), 1);
    assert_eq!(c.outbox(None).await.unwrap().as_array().unwrap().len(), 2);
    let err = c
        .command(json!({"action":"handoff","branch":"main","commit":"a".repeat(40),"team":"notes"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("no repository"),
        "path-bound teams cannot hand off commits: {err}"
    );
    // A session from the stray checkout cannot use either team, even by naming it.
    let err = c
        .command(json!({"action":"list","via":stray["label"],"team":"notes"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("not enrolled"), "{err}");
    c.enroll(stray["label"].as_str().unwrap(), true, Some("notes"))
        .unwrap();
    assert_eq!(
        c.resolve_team(&json!({"via":stray["label"]})).unwrap(),
        notes_id
    );
    let err = c
        .command(json!({"action":"list","via":stray["label"],"team":"app"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("different team"), "{err}");
}

#[tokio::test]
async fn the_last_member_can_leave_and_agents_are_released() {
    let tmp = TempDir::new().unwrap();
    let c = founder(&tmp);
    let notes = tmp.path().join("notes");
    std::fs::create_dir(&notes).unwrap();
    let key = c.create(None, &notes).await.unwrap();
    let id = key["team"].as_str().unwrap().to_owned();
    let a = c.attach("claude", &notes, None, None, None).unwrap()["agent"].clone();
    assert_eq!(a["team"], json!(id));
    c.leave(&id).await.unwrap();
    assert!(c.membership(&id).is_err());
    assert_eq!(
        c.agent(a["label"].as_str().unwrap()).unwrap()["enrolled"],
        false
    );
    assert_eq!(c.teams().unwrap().as_array().unwrap().len(), 0);
}

#[test]
fn a_single_team_database_upgrades_into_the_first_team_row() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("state");
    storage::identity(&dir, "Upgraded").unwrap();
    let founder = whatsai_core::crypto::Identity::generate("Old founder");
    let team = whatsai_core::governance::replay(
        &[founder
            .sign(whatsai_core::protocol::Governance {
                team: whatsai_core::protocol::id(),
                revision: 0,
                previous: String::new(),
                action: "create".into(),
                member: Some(founder.member().unwrap()),
                target: None,
                repository: Some("https://example.com/old/repo.git".into()),
                workspace: None,
            })
            .unwrap()],
        &founder.member().unwrap().id,
    )
    .unwrap();
    {
        let old =
            storage::database(&dir.join("client.db"), &storage::CLIENT_MIGRATIONS[..4]).unwrap();
        let authority = iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[3u8; 32]).public());
        for (k, v) in [
            ("team", serde_json::to_string(&team).unwrap()),
            ("authority", serde_json::to_string(&authority).unwrap()),
            ("secret", "7".repeat(64)),
            ("cursor", "12".into()),
        ] {
            old.execute("INSERT INTO config VALUES(?,?)", rusqlite::params![k, v])
                .unwrap();
        }
        old.execute("INSERT INTO agents(label,harness,workspace,created,last_seen,enrolled) VALUES('claude@x','claude','/x',1,1,1)", []).unwrap();
    }
    let c = Client::open(&dir, "Upgraded").unwrap();
    let m = c.membership(&team.id).unwrap();
    assert_eq!(m.cursor, 12);
    assert_eq!(
        m.team.workspace, "repo",
        "the name comes from the repository when the record has none"
    );
    assert_eq!(
        m.team.repository.as_deref(),
        Some("https://example.com/old/repo.git")
    );
    assert_eq!(
        c.agent("claude@x").unwrap()["team"],
        json!(team.id),
        "enrolled agents follow the team"
    );
    assert!(c.config("team").unwrap().is_none());
    assert_eq!(c.find_team("repo").unwrap(), team.id);
}

#[tokio::test]
async fn joining_with_a_key_you_already_hold_enrolls_that_workspace() {
    let tmp = TempDir::new().unwrap();
    let mut c = founder(&tmp);
    let copyk8 = tmp.path().join("copyk8");
    std::fs::create_dir(&copyk8).unwrap();
    let key = c.create(None, &copyk8).await.unwrap();
    let mailbox = tmp.path().join("mailbox");
    std::fs::create_dir(&mailbox).unwrap();
    let session = c.attach("claude", &mailbox, None, None, None).unwrap()["agent"].clone();
    assert_eq!(
        session["enrolled"], false,
        "another directory is not in the team by itself"
    );
    let result = c
        .command(
            json!({"action":"join","key":key["join"],"workspace":mailbox,"via":session["label"]}),
        )
        .await
        .unwrap();
    assert_eq!(result["state"], "enrolled");
    assert_eq!(result["workspace"], "copyk8");
    assert_eq!(result["agents"], json!([session["label"]]));
    let after = c.agent(session["label"].as_str().unwrap()).unwrap();
    assert_eq!(
        (after["enrolled"].as_bool(), after["team_name"].as_str()),
        (Some(true), Some("copyk8"))
    );
    assert_eq!(
        after["published"], true,
        "a deliberate join makes the joiner visible"
    );
    let visible = c.agent_presence(key["team"].as_str().unwrap()).unwrap();
    assert!(
        visible
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["label"] == session["label"])
    );
    assert_eq!(
        c.teams().unwrap().as_array().unwrap().len(),
        1,
        "no second membership was created"
    );
    assert!(
        c.command(json!({"action":"list","via":session["label"]}))
            .await
            .is_ok(),
        "the session can now act in the team"
    );
}

#[tokio::test]
async fn the_founding_session_is_visible_in_its_team() {
    let tmp = TempDir::new().unwrap();
    let mut c = founder(&tmp);
    let copyk8 = tmp.path().join("copyk8");
    std::fs::create_dir(&copyk8).unwrap();
    let codex = c.attach("codex", &copyk8, None, None, None).unwrap()["agent"].clone();
    let key = c
        .command(json!({"action":"create","workspace":copyk8,"via":codex["label"]}))
        .await
        .unwrap();
    assert_eq!(key["agents"], json!([codex["label"]]));
    let team = key["team"].as_str().unwrap();
    let visible = c.agent_presence(team).unwrap();
    assert_eq!(visible.as_array().unwrap().len(), 1);
    assert_eq!(visible[0]["label"], codex["label"]);
    assert_eq!(visible[0]["workspace"], "copyk8");
}

#[test]
fn people_and_agents_are_addressable_by_name() {
    use whatsai_core::{client::resolve_address, crypto::Identity, protocol::*};
    let alice = Identity::generate("alice").member().unwrap();
    let bob = Identity::generate("bob").member().unwrap();
    let bob2 = Identity::generate("bob").member().unwrap();
    let mut team = whatsai_core::governance::replay(
        &[Identity::generate("x")
            .sign(Governance {
                team: id(),
                revision: 0,
                previous: String::new(),
                action: "create".into(),
                member: None,
                target: None,
                repository: None,
                workspace: Some("t".into()),
            })
            .unwrap()],
        "",
    )
    .err()
    .map(|_| Team {
        id: id(),
        founder: alice.id.clone(),
        repository: None,
        workspace: "t".into(),
        revision: 0,
        members: [
            (alice.id.clone(), alice.clone()),
            (bob.id.clone(), bob.clone()),
            (bob2.id.clone(), bob2.clone()),
        ]
        .into(),
        admins: vec![alice.id.clone()],
        history: vec![],
        presence: Default::default(),
        endpoints: Default::default(),
        agents: Default::default(),
    })
    .unwrap();
    team.agents.insert(
        alice.id.clone(),
        json!([{"label":"claude@copyk8"},{"label":"codex@copyk8"}]),
    );
    team.agents
        .insert(bob.id.clone(), json!([{"label":"claude@copyk8"}]));
    assert_eq!(
        resolve_address(&team, "alice").unwrap(),
        (alice.id.clone(), None)
    );
    assert_eq!(
        resolve_address(&team, &alice.id).unwrap(),
        (alice.id.clone(), None)
    );
    assert_eq!(
        resolve_address(&team, "codex@copyk8").unwrap(),
        (alice.id.clone(), Some("codex@copyk8".into()))
    );
    assert_eq!(
        resolve_address(&team, "alice/claude@copyk8").unwrap(),
        (alice.id.clone(), Some("claude@copyk8".into()))
    );
    let err = resolve_address(&team, "claude@copyk8")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("several members"),
        "two members publish that label: {err}"
    );
    let err = resolve_address(&team, "bob").unwrap_err().to_string();
    assert!(
        err.contains("several members"),
        "two members are called bob: {err}"
    );
    assert_eq!(
        resolve_address(&team, &format!("{}/claude@copyk8", bob.id)).unwrap(),
        (bob.id.clone(), Some("claude@copyk8".into()))
    );
    assert!(resolve_address(&team, "carol").is_err());
    assert!(resolve_address(&team, "alice/nothing@here").is_err());
}

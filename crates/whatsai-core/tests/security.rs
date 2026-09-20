use serde_json::{Value, json};
use tempfile::TempDir;
use whatsai_core::{crypto::*, governance::*, protocol::*, service::Service, storage};
const SECRET: &str = "7f3a9c1e5b2d8e4f6a0c1b3d5e7f9a2b4c6d8e0f1a3b5c7d9e1f2a4b6c8d0e1f";
fn fixture_authority() -> iroh::EndpointAddr {
    iroh::EndpointAddr::new(iroh::SecretKey::from_bytes(&[9u8; 32]).public())
}

fn create(a: &Identity, team: &str) -> Signed<Governance> {
    a.sign(Governance {
        team: team.into(),
        revision: 0,
        previous: String::new(),
        action: "create".into(),
        member: Some(a.member().unwrap()),
        target: None,
        repository: Some("https://example.com/team/repo.git".into()),
        workspace: None,
    })
    .unwrap()
}
fn change(
    a: &Identity,
    t: &Team,
    action: &str,
    member: Option<Member>,
    target: Option<String>,
) -> Signed<Governance> {
    a.sign(Governance {
        team: t.id.clone(),
        revision: t.history.len(),
        previous: digest(&serde_json::to_vec(t.history.last().unwrap()).unwrap()),
        action: action.into(),
        member,
        target,
        repository: None,
        workspace: None,
    })
    .unwrap()
}
fn call(s: &Service, a: &Identity, op: Value) -> anyhow::Result<Value> {
    s.handle(a.sign(Request {
        version: VERSION,
        nonce: id(),
        timestamp: now(),
        operation: op,
    })?)
}
#[test]
fn encryption_rejects_tampering_and_outsiders() {
    let a = Identity::generate("Alice");
    let b = Identity::generate("Bob");
    let c = Identity::generate("Outsider");
    let mut members = vec![a.member().unwrap(), b.member().unwrap()];
    members.sort_by_key(|m| m.id.clone());
    let h = Header {
        version: VERSION,
        id: id(),
        team: id(),
        sender: a.member().unwrap().id,
        created: now(),
        kind: "message".into(),
        recipients: members.iter().map(|m| m.id.clone()).collect(),
    };
    let sealed = a.seal(h, b"secret message", &members).unwrap();
    assert_eq!(b.open(&sealed).unwrap(), b"secret message");
    assert!(c.open(&sealed).is_err());
    assert!(
        !serde_json::to_string(&sealed)
            .unwrap()
            .contains("secret message")
    );
    let mut changed = sealed.clone();
    changed.body.header.team = id();
    assert!(b.open(&changed).is_err());
    let mut changed = sealed;
    changed.body.ciphertext.push('A');
    assert!(b.open(&changed).is_err());
}
#[test]
fn promoted_admin_can_admit_without_creator_and_last_admin_is_protected() {
    let tmp = TempDir::new().unwrap();
    let s = Service::open(tmp.path(), 20_000_000).unwrap();
    let a = Identity::generate("Alice");
    let b = Identity::generate("Bob");
    let c = Identity::generate("Charlie");
    let tid = id();
    let mut t: Team = serde_json::from_value(
        call(
            &s,
            &a,
            json!({"method":"create","record":create(&a,&tid),"secret":SECRET}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(call(&s, &b, json!({"method":"team","team":tid})).is_err());
    assert_eq!(
        call(
            &s,
            &b,
            json!({"method":"request_join","team":tid,"member":b.member().unwrap(),"secret":SECRET})
        )
        .unwrap()["state"],
        "pending"
    );
    let admit = change(&a, &t, "admit", Some(b.member().unwrap()), None);
    t = serde_json::from_value(
        call(&s, &a, json!({"method":"govern","team":tid,"record":admit})).unwrap(),
    )
    .unwrap();
    assert!(call(&s,&a,json!({"method":"govern","team":tid,"record":change(&a,&t,"demote",None,Some(a.member().unwrap().id))})).is_err());
    let unauthorized = change(&b, &t, "promote", None, Some(b.member().unwrap().id));
    assert!(
        call(
            &s,
            &b,
            json!({"method":"govern","team":tid,"record":unauthorized})
        )
        .is_err()
    );
    let promote = change(&a, &t, "promote", None, Some(b.member().unwrap().id));
    t = serde_json::from_value(
        call(
            &s,
            &a,
            json!({"method":"govern","team":tid,"record":promote}),
        )
        .unwrap(),
    )
    .unwrap();
    call(
        &s,
        &c,
        json!({"method":"request_join","team":tid,"member":c.member().unwrap(),"secret":SECRET}),
    )
    .unwrap();
    let admit = change(&b, &t, "admit", Some(c.member().unwrap()), None);
    t = serde_json::from_value(
        call(&s, &b, json!({"method":"govern","team":tid,"record":admit})).unwrap(),
    )
    .unwrap();
    verify_team(&t, &a.member().unwrap().id).unwrap();
    assert!(t.members.contains_key(&c.member().unwrap().id));
    let mut forged = t.clone();
    forged
        .members
        .get_mut(&b.member().unwrap().id)
        .unwrap()
        .encryption_key = c.member().unwrap().encryption_key;
    assert!(verify_team(&forged, &a.member().unwrap().id).is_err());
}
#[test]
fn state_is_stable_and_future_schema_is_refused() {
    let tmp = TempDir::new().unwrap();
    let a = storage::identity(tmp.path(), "Alice").unwrap();
    let db = storage::database(&tmp.path().join("client.db"), storage::CLIENT_MIGRATIONS).unwrap();
    assert_eq!(
        a.member().unwrap().id,
        storage::identity(tmp.path(), "Ignored")
            .unwrap()
            .member()
            .unwrap()
            .id
    );
    db.pragma_update(None, "user_version", 99).unwrap();
    drop(db);
    assert!(storage::database(&tmp.path().join("client.db"), storage::CLIENT_MIGRATIONS).is_err());
    std::fs::remove_file(tmp.path().join("identity.json")).unwrap();
    assert!(storage::identity(tmp.path(), "Replacement").is_err());
}
#[test]
fn unsafe_remotes_and_filenames_are_rejected() {
    for remote in [
        "/home/user/repo",
        "https://token@example.com/repo",
        "https://example.com/repo?token=secret",
        "ssh://root@example.com/repo",
    ] {
        assert!(validate_remote(remote).is_err(), "{remote}");
    }
    for remote in [
        "git@example.com:org/repo.git",
        "https://example.com/org/repo.git",
        "ssh://git@example.com/org/repo.git",
    ] {
        validate_remote(remote).unwrap();
    }
    for name in ["../bad", "/bad", "..", "a\\b"] {
        assert!(
            Manifest {
                name: name.into(),
                size: 0,
                sha256: digest(b""),
                chunks: vec![]
            }
            .validate()
            .is_err()
        );
    }
}
#[test]
fn hpke_known_answer_receiver_vector() {
    use hpke::{Deserializable, Kem, OpModeR};
    type K = hpke::kem::X25519HkdfSha256;
    let v: Value = serde_json::from_str(include_str!("fixtures/hpke-base.json")).unwrap();
    let bytes = |s: &str| hex::decode(v[s].as_str().unwrap()).unwrap();
    let sk = <K as Kem>::PrivateKey::from_bytes(&bytes("skRm")).unwrap();
    let enc = <K as Kem>::EncappedKey::from_bytes(&bytes("enc")).unwrap();
    let mut ctx = hpke::setup_receiver::<hpke::aead::ChaCha20Poly1305, hpke::kdf::HkdfSha256, K>(
        &OpModeR::Base,
        &sk,
        &enc,
        &bytes("info"),
    )
    .unwrap();
    for e in v["encryptions"].as_array().unwrap() {
        let ct = hex::decode(e["ct"].as_str().unwrap()).unwrap();
        let aad = hex::decode(e["aad"].as_str().unwrap()).unwrap();
        assert_eq!(
            ctx.open(&ct, &aad).unwrap(),
            hex::decode(e["pt"].as_str().unwrap()).unwrap()
        );
    }
}

#[test]
fn replay_expiry_conflicts_and_revocation_are_enforced() {
    let tmp = TempDir::new().unwrap();
    let s = Service::open(tmp.path(), 20_000_000).unwrap();
    let a = Identity::generate("Alice");
    let b = Identity::generate("Bob");
    let c = Identity::generate("Charlie");
    let tid = id();
    let mut t: Team = serde_json::from_value(
        call(
            &s,
            &a,
            json!({"method":"create","record":create(&a,&tid),"secret":SECRET}),
        )
        .unwrap(),
    )
    .unwrap();
    call(
        &s,
        &b,
        json!({"method":"request_join","team":tid,"member":b.member().unwrap(),"secret":SECRET}),
    )
    .unwrap();
    let approval = change(&a, &t, "admit", Some(b.member().unwrap()), None);
    t = serde_json::from_value(
        call(
            &s,
            &a,
            json!({"method":"govern","team":tid,"record":approval.clone()}),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(
        call(
            &s,
            &a,
            json!({"method":"govern","team":tid,"record":approval})
        )
        .is_err()
    );
    let request = a
        .sign(Request {
            version: VERSION,
            nonce: id(),
            timestamp: now(),
            operation: json!({"method":"team","team":tid}),
        })
        .unwrap();
    s.handle(request.clone()).unwrap();
    assert!(s.handle(request).is_err());
    let request = a
        .sign(Request {
            version: VERSION,
            nonce: id(),
            timestamp: now() - 301,
            operation: json!({"method":"team","team":tid}),
        })
        .unwrap();
    assert!(s.handle(request).is_err());
    call(
        &s,
        &c,
        json!({"method":"request_join","team":tid,"member":c.member().unwrap(),"secret":SECRET}),
    )
    .unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("service.db")).unwrap();
    db.execute(
        "UPDATE requests SET expires=0 WHERE member=?",
        [c.member().unwrap().id],
    )
    .unwrap();
    assert_eq!(
        call(&s, &c, json!({"method":"join_status","team":tid})).unwrap()["state"],
        "expired"
    );
    assert!(call(&s,&a,json!({"method":"govern","team":tid,"record":change(&a,&t,"admit",Some(c.member().unwrap()),None)})).is_err());
    let members = t.members.values().cloned().collect::<Vec<_>>();
    let eid = id();
    let h = Header {
        version: VERSION,
        id: eid.clone(),
        team: tid.clone(),
        sender: a.member().unwrap().id,
        created: now(),
        kind: "message".into(),
        recipients: t.members.keys().cloned().collect(),
    };
    let env = a.seal(h, b"encrypted content", &members).unwrap();
    let put = call(&s, &a, json!({"method":"put","team":tid,"envelope":env})).unwrap();
    assert_eq!(
        put["seq"],
        call(&s, &a, json!({"method":"put","team":tid,"envelope":env})).unwrap()["seq"]
    );
    assert_eq!(
        call(&s, &b, json!({"method":"pull","team":tid,"cursor":0}))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );
    call(
        &s,
        &a,
        json!({"method":"authorize","team":tid,"event":eid,"recipient":b.member().unwrap().id}),
    )
    .unwrap();
    let revoke = change(&a, &t, "revoke", None, Some(b.member().unwrap().id));
    call(
        &s,
        &a,
        json!({"method":"govern","team":tid,"record":revoke}),
    )
    .unwrap();
    assert!(call(&s, &b, json!({"method":"pull","team":tid,"cursor":0})).is_err());
    assert!(
        call(
            &s,
            &a,
            json!({"method":"authorize","team":tid,"event":eid,"recipient":b.member().unwrap().id})
        )
        .is_err()
    );
}
#[test]
fn message_and_file_boundaries() {
    let mut e = Event {
        actor: "agent".into(),
        text: "x".repeat(MAX_TEXT),
        to: None,
        reply_to: None,
        root: id(),
        data: Value::Null,
        agent: None,
        to_agent: None,
    };
    e.validate().unwrap();
    e.text.push('x');
    assert!(e.validate().is_err());
    Manifest {
        name: "empty.bin".into(),
        size: 0,
        sha256: digest(b""),
        chunks: vec![],
    }
    .validate()
    .unwrap();
    let mut m = Manifest {
        name: "max.bin".into(),
        size: MAX_FILE,
        sha256: digest(b"test"),
        chunks: vec![digest(b"test"); 32],
    };
    m.validate().unwrap();
    m.size += 1;
    assert!(m.validate().is_err());
}
#[test]
fn worker_budget_survives_restart_and_inbox_default_does_not_launch() {
    use whatsai_core::client::Client;
    let tmp = TempDir::new().unwrap();
    let mut c = Client::open(tmp.path(), "Receiver").unwrap();
    let a = Identity::generate("Sender");
    let tid = id();
    let mut log = vec![create(&a, &tid)];
    let t = replay(&log, &a.member().unwrap().id).unwrap();
    log.push(change(
        &a,
        &t,
        "admit",
        Some(c.identity.member().unwrap()),
        None,
    ));
    let t = replay(&log, &a.member().unwrap().id).unwrap();
    c.install_team(&t, &fixture_authority(), &"7".repeat(64), None)
        .unwrap();
    let workspace = tmp.path().join("repo");
    std::fs::create_dir(&workspace).unwrap();
    let label = c.attach("codex", &workspace, None, None, None).unwrap()["agent"]["label"]
        .as_str()
        .unwrap()
        .to_owned();
    let adapter = tmp.path().join("adapter.js");
    std::fs::write(&adapter, "").unwrap();
    assert!(
        c.worker_command(
            &json!({"operation":"bind","agent":label,"adapter":adapter,"default":true})
        )
        .is_err(),
        "workers need an enrolled agent"
    );
    c.enroll(&label, true, None).unwrap();
    c.worker_command(&json!({"operation":"bind","agent":label,"adapter":adapter,"default":true}))
        .unwrap();
    let root = id();
    for i in 0..4 {
        let eid = id();
        let e = Event {
            actor: "agent".into(),
            text: "hello".into(),
            to: Some(c.identity.member().unwrap().id),
            reply_to: None,
            root: root.clone(),
            data: Value::Null,
            agent: None,
            to_agent: None,
        };
        let h = Header {
            version: VERSION,
            id: eid,
            team: tid.clone(),
            sender: a.member().unwrap().id,
            created: now(),
            kind: "message".into(),
            recipients: t.members.keys().cloned().collect(),
        };
        let env = a
            .seal(
                h,
                &serde_json::to_vec(&e).unwrap(),
                &t.members.values().cloned().collect::<Vec<_>>(),
            )
            .unwrap();
        c.receive(i, &env).unwrap();
    }
    assert!(c.claim_work().unwrap().is_none());
    c.worker_command(&json!({"operation":"enable","agent":label}))
        .unwrap();
    for i in 0..3 {
        let work = c.claim_work().unwrap().unwrap();
        assert_eq!(work.agent, label);
        if i == 0 {
            c.finish_work(&work, Ok(json!({"text":"x".repeat(MAX_TEXT+1)})))
                .unwrap();
        }
    }
    let failed: i64 =
        c.db.query_row(
            "SELECT count(*) FROM inbox WHERE dispatch='failed'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(failed, 1);
    drop(c);
    let mut c = Client::open(tmp.path(), "Receiver").unwrap();
    assert!(c.claim_work().unwrap().is_none());
    let count: i64 =
        c.db.query_row(
            "SELECT count(*) FROM inbox WHERE dispatch='interrupted'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 2);
}
#[test]
fn concurrent_approvals_admit_once() {
    use std::sync::Arc;
    let tmp = TempDir::new().unwrap();
    let service = Arc::new(Service::open(tmp.path(), 1_000_000).unwrap());
    let admin = Identity::generate("Admin");
    let applicant = Identity::generate("Applicant");
    let tid = id();
    let t: Team = serde_json::from_value(
        call(
            &service,
            &admin,
            json!({"method":"create","record":create(&admin,&tid),"secret":SECRET}),
        )
        .unwrap(),
    )
    .unwrap();
    call(
        &service,
        &applicant,
        json!({"method":"request_join","team":tid,"member":applicant.member().unwrap(),"secret":SECRET}),
    )
    .unwrap();
    let approval = change(&admin, &t, "admit", Some(applicant.member().unwrap()), None);
    let mut handles = vec![];
    for _ in 0..2 {
        let s = service.clone();
        let a = admin.clone();
        let approval = approval.clone();
        let tid = tid.clone();
        handles.push(std::thread::spawn(move || {
            call(
                &s,
                &a,
                json!({"method":"govern","team":tid,"record":approval}),
            )
            .is_ok()
        }));
    }
    assert_eq!(
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .filter(|ok| *ok)
            .count(),
        1
    );
}
#[test]
fn storage_quota_and_expiry_are_explicit() {
    let tmp = TempDir::new().unwrap();
    let service = Service::open(tmp.path(), 100).unwrap();
    let admin = Identity::generate("Admin");
    let tid = id();
    let t: Team = serde_json::from_value(
        call(
            &service,
            &admin,
            json!({"method":"create","record":create(&admin,&tid),"secret":SECRET}),
        )
        .unwrap(),
    )
    .unwrap();
    let eid = id();
    let h = Header {
        version: VERSION,
        id: eid.clone(),
        team: tid.clone(),
        sender: admin.member().unwrap().id,
        created: now(),
        kind: "message".into(),
        recipients: t.members.keys().cloned().collect(),
    };
    let env = admin
        .seal(
            h,
            b"content",
            &t.members.values().cloned().collect::<Vec<_>>(),
        )
        .unwrap();
    assert!(
        call(
            &service,
            &admin,
            json!({"method":"put","team":tid,"envelope":env})
        )
        .unwrap_err()
        .to_string()
        .contains("quota")
    );
    drop(service);
    let service = Service::open(tmp.path(), 1_000_000).unwrap();
    call(
        &service,
        &admin,
        json!({"method":"put","team":tid,"envelope":env}),
    )
    .unwrap();
    let db = rusqlite::Connection::open(tmp.path().join("service.db")).unwrap();
    db.execute("UPDATE events SET expires=0", []).unwrap();
    assert_eq!(
        call(
            &service,
            &admin,
            json!({"method":"receipts","team":tid,"event":eid})
        )
        .unwrap()["expired"],
        true
    );
    assert!(
        call(
            &service,
            &admin,
            json!({"method":"pull","team":tid,"cursor":0})
        )
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty()
    );
}
#[tokio::test]
async fn hung_worker_is_timed_out() {
    use whatsai_core::worker::{Binding, Work, execute};
    let tmp = TempDir::new().unwrap();
    let adapter = tmp.path().join("hang.js");
    std::fs::write(&adapter, "setInterval(() => {}, 1000);\n").unwrap();
    let work = Work {
        event: id(),
        sender: "synthetic".into(),
        sender_agent: None,
        team: id(),
        agent: "codex@test".into(),
        prompt: "test".into(),
        binding: Binding {
            harness: "codex".into(),
            cwd: tmp.path().to_string_lossy().into(),
            adapter: adapter.to_string_lossy().into(),
            enabled: true,
            limit: 3,
            timeout_secs: 0,
            session: None,
            default: false,
        },
    };
    assert!(
        execute(&work)
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out")
    );
}

#[test]
fn network_secret_gates_join_requests() {
    let tmp = TempDir::new().unwrap();
    let s = Service::open(tmp.path(), 20_000_000).unwrap();
    let a = Identity::generate("Founder");
    let b = Identity::generate("Applicant");
    let tid = id();
    assert!(
        call(
            &s,
            &a,
            json!({"method":"create","record":create(&a,&tid),"secret":"short"})
        )
        .is_err(),
        "malformed secrets are refused at creation"
    );
    call(
        &s,
        &a,
        json!({"method":"create","record":create(&a,&tid),"secret":SECRET}),
    )
    .unwrap();
    let wrong = SECRET.replace('7', "8");
    for bad in [json!(wrong), json!(""), Value::Null] {
        let err = call(
            &s,
            &b,
            json!({"method":"request_join","team":tid,"member":b.member().unwrap(),"secret":bad}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("network secret rejected"), "{err}");
    }
    assert_eq!(
        call(
            &s,
            &b,
            json!({"method":"request_join","team":tid,"member":b.member().unwrap(),"secret":SECRET})
        )
        .unwrap()["state"],
        "pending"
    );
    assert!(secret_matches(SECRET, SECRET) && !secret_matches(SECRET, &wrong));
}

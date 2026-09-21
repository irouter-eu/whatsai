//! Deterministic text views of daemon results: one table per command, the same rows in the
//! terminal client, in `--table` output, and in the slash commands. No model in the loop.
use serde_json::Value;

pub fn short(id: &str) -> String {
    id.chars().take(8).collect()
}
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn when(ts: i64) -> String {
    let age = whatsai_core::protocol::now() - ts;
    match age {
        a if a < 0 => "now".into(),
        a if a < 60 => format!("{a}s ago"),
        a if a < 3600 => format!("{}m ago", a / 60),
        a if a < 86400 => format!("{}h ago", a / 3600),
        a => format!("{}d ago", a / 86400),
    }
}
/// Left-aligned columns padded to the widest cell.
pub fn table(headers: &[&str], rows: &[Vec<String>]) -> Vec<String> {
    let cols = headers.len();
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in rows {
        for (i, cell) in row.iter().enumerate().take(cols) {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }
    let fmt = |cells: &[String]| {
        cells
            .iter()
            .enumerate()
            .take(cols)
            .map(|(i, c)| format!("{:<w$}", c, w = widths[i]))
            .collect::<Vec<_>>()
            .join("  ")
            .trim_end()
            .to_owned()
    };
    let mut out = vec![fmt(&headers
        .iter()
        .map(|h| h.to_string())
        .collect::<Vec<_>>())];
    out.push(
        widths
            .iter()
            .map(|w| "-".repeat(*w))
            .collect::<Vec<_>>()
            .join("  "),
    );
    for row in rows {
        out.push(fmt(row));
    }
    out
}
pub fn teams(v: &Value) -> Vec<String> {
    let rows: Vec<Vec<String>> = v
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| {
                    vec![
                        s(t, "workspace").to_owned(),
                        s(t, "state").to_owned(),
                        s(t, "role").to_owned(),
                        t["members"]
                            .as_u64()
                            .map(|n| n.to_string())
                            .unwrap_or_default(),
                        s(t, "repository").to_owned(),
                        short(s(t, "id")),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return vec!["No teams. Create one here with `whatsai create`, or join with a key.".into()];
    }
    table(
        &["WORKSPACE", "STATE", "ROLE", "MEMBERS", "REPOSITORY", "ID"],
        &rows,
    )
}
/// The team as a flat list of participants: one row per published session, named as the team
/// addresses it (`alice/claude`), carrying its person's role and fingerprint. A member with no
/// published session gets a single row under their name so admins can still act on them.
pub fn members(team_v: &Value) -> Vec<String> {
    let team: Option<whatsai_core::protocol::Team> = serde_json::from_value(team_v.clone()).ok();
    let now = whatsai_core::protocol::now();
    let mut rows = vec![];
    if let Some(team) = &team {
        let handles = whatsai_core::protocol::member_handles(team);
        let participants = whatsai_core::protocol::participants(team);
        let mut members: Vec<&whatsai_core::protocol::Member> = team.members.values().collect();
        members.sort_by_key(|m| handles.get(&m.id).cloned().unwrap_or_default());
        for m in members {
            let role = if m.id == team.founder {
                "founder, admin"
            } else if team.admins.contains(&m.id) {
                "admin"
            } else {
                "member"
            };
            let mine: Vec<&whatsai_core::protocol::Participant> =
                participants.iter().filter(|p| p.member == m.id).collect();
            if mine.is_empty() {
                let state = match team.presence.get(&m.id) {
                    Some(t) if now - t < 15 => "online, no sessions".to_owned(),
                    Some(t) => format!("seen {}, no sessions", when(*t)),
                    None => "never seen".into(),
                };
                rows.push(vec![
                    handles
                        .get(&m.id)
                        .cloned()
                        .unwrap_or_else(|| m.name.clone()),
                    role.into(),
                    state,
                    short(&m.id),
                    String::new(),
                ]);
                continue;
            }
            for p in mine {
                let extra = team.agents[&m.id]
                    .as_array()
                    .and_then(|a| a.iter().find(|x| x["label"] == p.label))
                    .map(|a| {
                        format!("{} {}", s(a, "workspace"), s(a, "repository"))
                            .trim()
                            .to_owned()
                    })
                    .unwrap_or_default();
                rows.push(vec![
                    p.handle.clone(),
                    role.into(),
                    if p.online { "online" } else { "offline" }.into(),
                    short(&m.id),
                    extra,
                ]);
            }
        }
    }
    let mut out = vec![format!(
        "Team {} ({}){}",
        s(team_v, "workspace"),
        short(s(team_v, "id")),
        team_v["repository"]
            .as_str()
            .map(|r| format!(" on {r}"))
            .unwrap_or_default()
    )];
    out.extend(table(
        &["ADDRESS", "ROLE", "STATE", "FINGERPRINT", "WORKSPACE"],
        &rows,
    ));
    out
}
pub fn requests(v: &Value) -> Vec<String> {
    let rows: Vec<Vec<String>> = v
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    vec![
                        s(&r["member"], "name").to_owned(),
                        s(r, "state").to_owned(),
                        s(&r["member"], "id").to_owned(),
                        r["expires"]
                            .as_i64()
                            .map(|t| {
                                format!(
                                    "expires in {}",
                                    when(2 * whatsai_core::protocol::now() - t).replace(" ago", "")
                                )
                            })
                            .unwrap_or_default(),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return vec!["No join requests.".into()];
    }
    table(&["NAME", "STATE", "FINGERPRINT", "EXPIRES"], &rows)
}
pub fn agents(v: &Value) -> Vec<String> {
    let rows: Vec<Vec<String>> = v
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    let unread = format!(
                        "{}+{}",
                        x["unread"]["addressed"].as_i64().unwrap_or(0),
                        x["unread"]["shared"].as_i64().unwrap_or(0)
                    );
                    vec![
                        s(x, "label").to_owned(),
                        x["team_name"].as_str().unwrap_or("-").to_owned(),
                        if x["retired"] == true {
                            "retired"
                        } else if x["published"] == true {
                            "published"
                        } else if x["enrolled"] == true {
                            "enrolled"
                        } else {
                            "private"
                        }
                        .into(),
                        if x["online"] == true {
                            format!("online x{}", x["sessions"].as_i64().unwrap_or(0))
                        } else {
                            format!("seen {}", when(x["last_seen"].as_i64().unwrap_or(0)))
                        },
                        unread,
                        if x["worker"].is_null() {
                            String::new()
                        } else if x["worker"]["enabled"] == true {
                            "worker on".into()
                        } else {
                            "worker paused".into()
                        },
                        s(x, "workspace").to_owned(),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return vec![
            "No agents yet. One appears the first time a harness session runs in a checkout."
                .into(),
        ];
    }
    table(
        &[
            "AGENT",
            "TEAM",
            "VISIBILITY",
            "STATE",
            "UNREAD",
            "WORKER",
            "PATH",
        ],
        &rows,
    )
}
/// Inbox rows. `team` is the `list` result when known, so senders and recipients show as the
/// team addresses them (`alice`, `alice/claude`); otherwise fingerprints and labels.
pub fn inbox(v: &Value, team: &Value) -> Vec<String> {
    let parsed: Option<whatsai_core::protocol::Team> = serde_json::from_value(team.clone()).ok();
    let handles = parsed
        .as_ref()
        .map(whatsai_core::protocol::member_handles)
        .unwrap_or_default();
    let person = |id: &str| -> String {
        handles
            .get(id)
            .cloned()
            .or_else(|| team["members"][id]["name"].as_str().map(str::to_owned))
            .unwrap_or_else(|| short(id))
    };
    let session = |id: &str, label: &str| -> String {
        parsed
            .as_ref()
            .and_then(|t| whatsai_core::protocol::handle_of(t, id, label))
            .unwrap_or_else(|| format!("{}/{label}", person(id)))
    };
    let rows: Vec<Vec<String>> = v
        .as_array()
        .map(|a| {
            a.iter()
                .map(|m| {
                    let sender = s(m, "sender");
                    let from = match m["event"]["agent"].as_str() {
                        Some(label) => session(sender, label),
                        None => person(sender),
                    };
                    let to = match (m["event"]["to"].as_str(), m["event"]["to_agent"].as_str()) {
                        (None, _) => "everyone".to_owned(),
                        (Some(id), Some(label)) => session(id, label),
                        (Some(id), None) => person(id),
                    };
                    let text = match s(m, "kind") {
                        "message" => s(&m["event"], "text").to_owned(),
                        "status" => format!(
                            "[status {}] {}",
                            s(&m["event"]["data"], "state"),
                            s(&m["event"], "text")
                        ),
                        "file" => format!(
                            "[file {} {} bytes]",
                            s(&m["event"]["data"], "name"),
                            m["event"]["data"]["size"]
                        ),
                        "handoff" => format!(
                            "[handoff {} @ {}] {}",
                            s(&m["event"]["data"], "branch"),
                            short(s(&m["event"]["data"], "commit")),
                            s(&m["event"], "text")
                        ),
                        other => format!("[{other}]"),
                    };
                    vec![
                        when(m["created"].as_i64().unwrap_or(0)),
                        from,
                        to,
                        text.replace('\n', " "),
                        short(s(m, "id")),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    if rows.is_empty() {
        return vec!["Inbox is empty.".into()];
    }
    table(&["WHEN", "FROM", "TO", "MESSAGE", "EVENT"], &rows)
}
pub fn version(v: &Value) -> Vec<String> {
    let mut rows = vec![];
    // The CLI report nests the daemon's own report under "daemon".
    let nested = v["daemon"].is_object();
    for key in [
        "cli", "plugin", "adapter", "daemon", "database", "protocol", "state", "agent",
    ] {
        let val = match (&v[key], nested) {
            (Value::Object(_), true) => v["daemon"]["daemon"].clone(),
            (Value::Null, true) => v["daemon"][key].clone(),
            (x, _) => x.clone(),
        };
        let val = match val {
            Value::Null => continue,
            Value::String(x) => x,
            other => other.to_string(),
        };
        rows.push(vec![key.to_owned(), val]);
    }
    let mut out = table(&["COMPONENT", "VERSION"], &rows);
    if v["mismatch"] == true || v["notice"].is_string() {
        out.push(
            v["notice"]
                .as_str()
                .unwrap_or("versions differ: restart the daemon after installing")
                .to_owned(),
        );
    }
    out
}
/// The table for a command result, when one exists.
pub fn render(action: &str, result: &Value, extra: &Value) -> Option<Vec<String>> {
    Some(match action {
        "teams" => teams(result),
        "list" => members(result),
        "requests" => requests(result),
        "agents" => agents(result),
        "inbox" | "files" => inbox(result, extra),
        "version" => version(result),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    /// A team as `list` returns it, with a real create record so member handles derive.
    fn fixture_team() -> Value {
        use whatsai_core::{crypto::Identity, governance::replay, protocol::*};
        let a = Identity::generate("aurelien");
        let b = Identity::generate("bob");
        let mut alice = a.member().unwrap();
        alice.id = alice.id.clone();
        let create = a
            .sign(Governance {
                team: "3e4d9441-b861-41f3-a393-16d8af812156".into(),
                revision: 0,
                previous: String::new(),
                action: "create".into(),
                member: Some(alice.clone()),
                target: None,
                repository: Some("https://x/y.git".into()),
                workspace: Some("whatsai".into()),
            })
            .unwrap();
        let previous = digest(&serde_json::to_vec(&create).unwrap());
        let admit = a
            .sign(Governance {
                team: "3e4d9441-b861-41f3-a393-16d8af812156".into(),
                revision: 1,
                previous,
                action: "admit".into(),
                member: Some(b.member().unwrap()),
                target: None,
                repository: None,
                workspace: None,
            })
            .unwrap();
        let mut team = replay(&[create, admit], &alice.id).unwrap();
        team.presence.insert(alice.id.clone(), now());
        team.agents.insert(alice.id.clone(), json!([{"label":"claude@whatsai","harness":"claude","online":true,"workspace":"whatsai","repository":"https://x/y.git"}]));
        let mut v = serde_json::to_value(&team).unwrap();
        // The tests below name members by fixed ids; rewrite the generated ids to those.
        let text = serde_json::to_string(&v)
            .unwrap()
            .replace(&alice.id, "aaaa1111bbbb")
            .replace(&b.member().unwrap().id, "cccc2222dddd");
        v = serde_json::from_str(&text).unwrap();
        v
    }
    #[test]
    fn tables_are_flat_participants() {
        let team = fixture_team();
        let lines = members(&team);
        assert_eq!(lines[0], "Team whatsai (3e4d9441) on https://x/y.git");
        assert!(lines[1].starts_with("ADDRESS"));
        assert!(
            lines[3].starts_with("aurelien/claude")
                && lines[3].contains("founder, admin")
                && lines[3].contains("online")
                && lines[3].contains("aaaa1111"),
            "{}",
            lines[3]
        );
        assert!(
            lines[4].starts_with("bob")
                && lines[4].contains("member")
                && lines[4].contains("never seen"),
            "a member with no sessions keeps one row: {}",
            lines[4]
        );
        assert_eq!(lines.len(), 5, "no person rows above sessions: {lines:?}");
    }
    #[test]
    fn inbox_rows_describe_every_kind() {
        let team = fixture_team();
        let inbox_v = json!([
            {"id":"e1","sender":"aaaa1111bbbb","kind":"message","created":whatsai_core::protocol::now()-30,"event":{"text":"hi\nthere","to":null,"agent":"claude@whatsai"}},
            {"id":"e2","sender":"cccc2222dddd","kind":"status","created":0,"event":{"text":"waiting","to":"aaaa1111bbbb","to_agent":"claude@whatsai","data":{"state":"blocked"}}},
            {"id":"e3","sender":"cccc2222dddd","kind":"file","created":0,"event":{"text":"Shared a file","data":{"name":"a.bin","size":12}}},
        ]);
        let parsed: Result<whatsai_core::protocol::Team, _> = serde_json::from_value(team.clone());
        assert!(parsed.is_ok(), "fixture parses: {:?}", parsed.err());
        let lines = inbox(&inbox_v, &team);
        assert!(
            lines[2].contains("aurelien/claude")
                && lines[2].contains("everyone")
                && lines[2].contains("hi there"),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].contains("bob")
                && lines[3].contains("[status blocked] waiting")
                && lines[3].contains("aurelien/claude"),
            "{}",
            lines[3]
        );
        assert!(lines[4].contains("[file a.bin 12 bytes]"));
        assert_eq!(
            inbox(&json!([]), &json!({})),
            vec!["Inbox is empty.".to_owned()]
        );
    }
    #[test]
    fn agents_and_teams_and_version_render() {
        let a = json!([{"label":"claude@x","team_name":"x","published":true,"enrolled":true,"online":true,"sessions":2,"unread":{"addressed":1,"shared":0},"worker":null,"workspace":"/p/x","last_seen":0}]);
        let lines = agents(&a);
        assert!(
            lines[2].contains("published")
                && lines[2].contains("online x2")
                && lines[2].contains("1+0")
        );
        let t = json!([{"workspace":"x","state":"member","role":"admin","members":2,"repository":null,"id":"abcdefgh-1"}]);
        assert!(teams(&t)[2].starts_with("x") && teams(&t)[2].contains("abcdefgh"));
        let v = json!({"cli":"0.8.0","daemon":{"daemon":"0.7.1","database":5},"notice":"restart"});
        let lines = version(&v);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("daemon") && l.contains("0.7.1"))
        );
        assert_eq!(lines.last().unwrap(), "restart");
        assert!(render("nothing", &json!({}), &json!({})).is_none());
    }
}

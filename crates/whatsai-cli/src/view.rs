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
/// Members and their published agents, from a `list` result.
pub fn members(team: &Value) -> Vec<String> {
    let admins: Vec<&str> = team["admins"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let now = whatsai_core::protocol::now();
    let mut rows = vec![];
    if let Some(members) = team["members"].as_object() {
        let mut members: Vec<(&String, &Value)> = members.iter().collect();
        members.sort_by_key(|(_, m)| s(m, "name").to_owned());
        for (id, m) in members {
            let role = if id.as_str() == s(team, "founder") {
                "founder, admin"
            } else if admins.contains(&id.as_str()) {
                "admin"
            } else {
                "member"
            };
            let seen = team["presence"][id].as_i64();
            let state = match seen {
                Some(t) if now - t < 15 => "online".to_owned(),
                Some(t) => format!("seen {}", when(t)),
                None => "never seen".into(),
            };
            rows.push(vec![
                s(m, "name").to_owned(),
                role.into(),
                state,
                short(id),
                String::new(),
            ]);
            if let Some(agents) = team["agents"][id].as_array() {
                for a in agents {
                    let online = if a["online"] == true {
                        "online"
                    } else {
                        "offline"
                    };
                    rows.push(vec![
                        format!("  {}", s(a, "label")),
                        "agent".into(),
                        online.into(),
                        String::new(),
                        format!("{} {}", s(a, "workspace"), s(a, "repository"))
                            .trim()
                            .to_owned(),
                    ]);
                }
            }
        }
    }
    let mut out = vec![format!(
        "Team {} ({}){}",
        s(team, "workspace"),
        short(s(team, "id")),
        team["repository"]
            .as_str()
            .map(|r| format!(" on {r}"))
            .unwrap_or_default()
    )];
    out.extend(table(
        &["NAME", "ROLE", "STATE", "FINGERPRINT", "WORKSPACE"],
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
/// Inbox rows; `names` maps fingerprints to display names when known.
pub fn inbox(v: &Value, names: &Value) -> Vec<String> {
    let rows: Vec<Vec<String>> = v
        .as_array()
        .map(|a| {
            a.iter()
                .map(|m| {
                    let sender = s(m, "sender");
                    let who = names[sender]["name"]
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| short(sender));
                    let from = match m["event"]["agent"].as_str() {
                        Some(label) => format!("{who}/{label}"),
                        None => who,
                    };
                    let to = match (m["event"]["to"].as_str(), m["event"]["to_agent"].as_str()) {
                        (None, _) => "everyone".to_owned(),
                        (Some(_), Some(label)) => label.to_owned(),
                        (Some(id), None) => names[id]["name"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| short(id)),
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
    #[test]
    fn tables_align_and_name_people_and_agents() {
        let team = json!({
            "id":"3e4d9441-b861-41f3-a393-16d8af812156","workspace":"whatsai","repository":"https://x/y.git",
            "founder":"aaaa1111bbbb","admins":["aaaa1111bbbb"],
            "members":{"aaaa1111bbbb":{"name":"aurelien"},"cccc2222dddd":{"name":"bob"}},
            "presence":{"aaaa1111bbbb": whatsai_core::protocol::now()},
            "agents":{"aaaa1111bbbb":[{"label":"claude@whatsai","online":true,"workspace":"whatsai","repository":"https://x/y.git"}]}
        });
        let lines = members(&team);
        assert_eq!(lines[0], "Team whatsai (3e4d9441) on https://x/y.git");
        assert!(lines[1].starts_with("NAME"));
        assert!(
            lines[3].starts_with("aurelien")
                && lines[3].contains("founder, admin")
                && lines[3].contains("online")
        );
        assert!(
            lines[4].starts_with("  claude@whatsai")
                && lines[4].contains("agent")
                && lines[4].contains("online")
        );
        assert!(lines[5].starts_with("bob") && lines[5].contains("never seen"));
        let widths: std::collections::HashSet<usize> = lines[1..]
            .iter()
            .map(|l| l.find("ROLE").or(l.find("founder")).unwrap_or(0))
            .collect();
        assert!(widths.len() <= 3, "columns line up: {lines:?}");
    }
    #[test]
    fn inbox_rows_describe_every_kind() {
        let names = json!({"aaaa1111":{"name":"alice"}});
        let inbox_v = json!([
            {"id":"e1","sender":"aaaa1111","kind":"message","created":whatsai_core::protocol::now()-30,"event":{"text":"hi\nthere","to":null,"agent":"codex@app"}},
            {"id":"e2","sender":"zzzz","kind":"status","created":0,"event":{"text":"waiting","to":"aaaa1111","to_agent":"claude@app","data":{"state":"blocked"}}},
            {"id":"e3","sender":"zzzz","kind":"file","created":0,"event":{"text":"Shared a file","data":{"name":"a.bin","size":12}}},
        ]);
        let lines = inbox(&inbox_v, &names);
        assert!(
            lines[2].contains("alice/codex@app")
                && lines[2].contains("everyone")
                && lines[2].contains("hi there")
        );
        assert!(lines[3].contains("[status blocked] waiting") && lines[3].contains("claude@app"));
        assert!(lines[4].contains("[file a.bin 12 bytes]"));
        assert_eq!(
            inbox(&json!([]), &names),
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

use crate::{crypto::verify, protocol::*};
use anyhow::{Result, bail, ensure};
use std::collections::BTreeMap;

pub fn replay(history: &[Signed<Governance>], founder: &str) -> Result<Team> {
    ensure!(!history.is_empty(), "empty governance history");
    let mut team = Team {
        id: history[0].body.team.clone(),
        founder: founder.into(),
        repository: None,
        workspace: String::new(),
        revision: 0,
        members: BTreeMap::new(),
        admins: vec![],
        history: vec![],
        presence: BTreeMap::new(),
        endpoints: BTreeMap::new(),
        agents: BTreeMap::new(),
    };
    valid_id(&team.id)?;
    let mut previous = String::new();
    for (i, record) in history.iter().enumerate() {
        verify(record)?;
        let g = &record.body;
        ensure!(
            g.team == team.id && g.revision == i && g.previous == previous,
            "invalid governance chain"
        );
        if i == 0 {
            ensure!(
                g.action == "create" && record.signer == founder,
                "invalid founder"
            );
            let m = g
                .member
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("missing founder"))?;
            ensure!(m.id == founder, "founder mismatch");
            validate_member(m)?;
            team.repository = g.repository.clone();
            if let Some(remote) = &team.repository {
                validate_remote(remote)?;
            }
            team.workspace = match &g.workspace {
                Some(name) => name.clone(),
                None => team
                    .repository
                    .as_deref()
                    .and_then(|r| r.trim_end_matches('/').rsplit(['/', ':']).next())
                    .map(|n| n.trim_end_matches(".git").to_owned())
                    .filter(|n| !n.is_empty())
                    .ok_or_else(|| anyhow::anyhow!("a team needs a repository or a workspace"))?,
            };
            validate_workspace_name(&team.workspace)?;
            team.members.insert(m.id.clone(), m.clone());
            team.admins.push(m.id.clone());
        } else {
            ensure!(!team.members.is_empty(), "team is closed");
            if g.action != "leave" {
                ensure!(team.admins.contains(&record.signer), "admin required");
            }
            match g.action.as_str() {
                "admit" => {
                    let m = g
                        .member
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("missing member"))?;
                    validate_member(m)?;
                    ensure!(!team.members.contains_key(&m.id), "already a member");
                    team.members.insert(m.id.clone(), m.clone());
                }
                "promote" | "demote" | "revoke" | "leave" => {
                    let target = g
                        .target
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("missing target"))?;
                    ensure!(team.members.contains_key(target), "unknown member");
                    if g.action == "leave" {
                        ensure!(target == &record.signer, "can only leave as yourself");
                    }
                    if g.action == "promote" {
                        ensure!(!team.admins.contains(target), "already admin");
                        team.admins.push(target.clone());
                    } else {
                        if team.admins.contains(target) {
                            ensure!(
                                team.admins.len() > 1
                                    || (team.members.len() == 1 && g.action == "leave"),
                                "last admin: promote another member first"
                            );
                            team.admins.retain(|x| x != target);
                        }
                        if g.action != "demote" {
                            team.members.remove(target);
                        }
                    }
                }
                _ => bail!("unknown governance action"),
            }
        }
        previous = digest(&serde_json::to_vec(record)?);
        team.revision = i;
        team.history.push(record.clone());
    }
    Ok(team)
}
pub fn verify_team(team: &Team, founder: &str) -> Result<()> {
    let expected = replay(&team.history, founder)?;
    ensure!(
        expected.id == team.id
            && expected.members == team.members
            && expected.admins == team.admins
            && expected.repository == team.repository
            && expected.workspace == team.workspace
            && expected.revision == team.revision,
        "roster differs from governance history"
    );
    Ok(())
}
pub fn validate_member(m: &Member) -> Result<()> {
    ensure!(
        !m.name.trim().is_empty() && m.name.len() <= 80 && !m.name.chars().any(char::is_control),
        "invalid display name"
    );
    ensure!(
        hex::decode(&m.id)?.len() == 32 && hex::decode(&m.encryption_key)?.len() == 32,
        "invalid member keys"
    );
    Ok(())
}

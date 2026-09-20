mod ui;
mod view;
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(Parser)]
#[command(version, about = "Private teamwork for people and their coding agents")]
struct Args {
    /// State directory; defaults to ~/.local/share/whatsai, or the profile's directory.
    #[arg(long, env = "WHATSAI_STATE", global = true)]
    state: Option<PathBuf>,
    /// Which person you are on a shared OS account: selects ~/.local/share/whatsai/profiles/NAME.
    #[arg(long, env = "WHATSAI_PROFILE", global = true)]
    profile: Option<String>,
    /// Which team a command means: id, workspace name, or repository. Defaults to the team the
    /// current directory is bound to, or the only team you have.
    #[arg(long, global = true)]
    team: Option<String>,
    /// Print a fixed table instead of JSON, for commands that have one.
    #[arg(long, global = true)]
    table: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Start {
        /// Display name for a new identity; ignored once an identity exists.
        #[arg(long, env = "WHATSAI_NAME", default_value_t = default_name())]
        name: String,
    },
    Register,
    Health,
    /// Versions of this CLI, the running daemon, and its database schema.
    Version,
    /// Teams you belong to, are joining, or are still creating.
    Teams,
    /// The terminal client: teams, members, inbox, requests and agents, live, with fixed keys.
    Ui,
    /// Profiles with an identity on this machine (people sharing this OS account).
    Profiles,
    Stop,
    Sync,
    Invite,
    List,
    Requests,
    /// Read the inbox, optionally as one agent sees it.
    Inbox {
        /// Only messages addressed to this agent or shared with everyone.
        #[arg(long, conflicts_with = "harness")]
        agent: Option<String>,
        /// The agent for this harness in the current directory, without knowing its label.
        #[arg(long)]
        harness: Option<String>,
        /// Only what that agent has not marked read.
        #[arg(long)]
        unread: bool,
    },
    Outbox,
    Files,
    /// Agents registered under this identity and their live sessions.
    Agents,
    /// Manage one agent: harness@workspace participants that outlive sessions.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
    AcceptHandoff {
        event: String,
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        directory: PathBuf,
    },
    /// Create a team bound to a workspace; this daemon becomes the network's authority.
    Create {
        /// Credential-free Git remote the team works on. Optional: without one the team is
        /// bound to the workspace directory alone.
        #[arg(long)]
        repository: Option<String>,
        /// The workspace directory; its name becomes the team's name.
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
    },
    /// Request admission with a join key from an existing member.
    Join {
        key: String,
        /// The local directory that will work in this team.
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
    },
    JoinStatus,
    Approve {
        member: String,
    },
    Reject {
        member: String,
    },
    Promote {
        member: String,
    },
    Demote {
        member: String,
    },
    Revoke {
        member: String,
    },
    Leave,
    Send {
        text: String,
        /// Recipient: a member's name or fingerprint, one of their published agents by label
        /// (claude@repo), or NAME/LABEL when a label is not unique in the team.
        #[arg(long)]
        to: Option<String>,
        /// One of the recipient's agents, e.g. claude@repo; needs --to.
        #[arg(long, requires = "to")]
        to_agent: Option<String>,
        #[arg(long)]
        reply_to: Option<String>,
        /// Label the message as written by an agent.
        #[arg(long)]
        agent: bool,
        /// Send as this local agent (implies --agent).
        #[arg(long = "as")]
        as_agent: Option<String>,
    },
    Share {
        path: PathBuf,
        #[arg(long)]
        agent: bool,
        #[arg(long = "as")]
        as_agent: Option<String>,
    },
    Download {
        file: String,
        #[arg(long)]
        directory: PathBuf,
        #[arg(long, hide = true)]
        max_chunks: Option<usize>,
    },
    Status {
        #[arg(long = "set")]
        work_state: Option<String>,
        #[arg(long)]
        description: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        #[arg(long)]
        commit: Option<String>,
        /// Report status for this local agent rather than yourself.
        #[arg(long = "as")]
        as_agent: Option<String>,
    },
    Handoff {
        #[arg(long)]
        branch: String,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        description: Option<String>,
        #[arg(long = "as")]
        as_agent: Option<String>,
    },
    /// Structured local adapter API; request must be passed on standard input.
    Rpc,
}
#[derive(Subcommand)]
enum AgentCommand {
    /// Register a session for a harness in a workspace, creating the agent on first use.
    Attach {
        #[arg(long)]
        harness: String,
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        pid: Option<i64>,
    },
    Heartbeat {
        lease: String,
    },
    Detach {
        lease: String,
    },
    Show {
        agent: String,
    },
    /// Let the team see and address this agent. Nothing is published without this.
    Publish {
        agent: String,
    },
    /// Hide this agent from the team again; sessions and history stay.
    Unpublish {
        agent: String,
    },
    /// Let this agent's sessions take part in a team (read, send, sync). Checkouts matching a
    /// team enroll themselves unless auto-enroll is off.
    Enroll {
        agent: String,
        /// The team, when the agent's workspace does not identify it.
        #[arg(long)]
        into: Option<String>,
    },
    /// Cut this agent's sessions off from the team; unpublishes too.
    Unenroll {
        agent: String,
    },
    /// Whether checkouts of the team's own repository enroll themselves on attach.
    AutoEnroll {
        #[arg(value_parser = ["off", "team-repo"])]
        mode: String,
    },
    /// Whether checkouts of the team's own repository publish themselves on attach.
    AutoPublish {
        #[arg(value_parser = ["off", "team-repo"])]
        mode: String,
    },
    /// Stop offering this agent to the team; its history stays.
    Retire {
        agent: String,
    },
    /// Move an agent to another checkout so its label and queue follow the work.
    Adopt {
        agent: String,
        #[arg(long)]
        workspace: PathBuf,
    },
    /// Unread counts for an agent, by label or by harness and workspace.
    Unread {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long, required_unless_present = "agent")]
        harness: Option<String>,
        #[arg(long, default_value = ".")]
        workspace: PathBuf,
    },
    MarkRead {
        agent: String,
    },
}
#[derive(Subcommand)]
enum WorkerCommand {
    /// Bind an automatic-reply worker to an agent; harness and cwd default to the agent's own.
    Bind {
        #[arg(long)]
        agent: String,
        #[arg(long)]
        adapter: PathBuf,
        #[arg(long)]
        harness: Option<String>,
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Also answer messages sent to you with no agent named.
        #[arg(long)]
        default: bool,
    },
    Enable {
        agent: String,
    },
    Pause {
        agent: String,
    },
    Unbind {
        agent: String,
    },
    Status,
    Reset {
        agent: String,
        root: String,
    },
}
/// New identities default to the local account name so automatic starts never create a "Member".
fn default_name() -> String {
    ["WHATSAI_NAME", "USER", "LOGNAME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|v| v.trim().to_string())
        .find(|v| !v.is_empty() && v.len() <= 80 && !v.chars().any(char::is_control))
        .unwrap_or_else(|| "Member".into())
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let state = match args.state {
        Some(state) => state,
        None => whatsai_core::storage::default_state_for(args.profile.as_deref())?,
    };
    let command = match args.command {
        Command::Start { name } => {
            if let Ok(result) =
                whatsai_core::daemon::request(&state, json!({"action":"health"})).await
            {
                println!("{}", serde_json::to_string_pretty(&result)?);
                return Ok(());
            }
            // Fail before spawning when the socket path can never bind, with the same message the daemon gives.
            whatsai_core::daemon::socket_path(&state)?;
            whatsai_core::storage::private_dir(&state)?;
            use std::os::unix::fs::OpenOptionsExt;
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(state.join("daemon.log"))?;
            let log_path = state.join("daemon.log");
            let written_before = std::fs::metadata(&log_path).map(|m| m.len()).unwrap_or(0);
            let binary = std::env::current_exe()?.with_file_name("whatsai-daemon");
            let mut child = std::process::Command::new(binary)
                .args([
                    "--state",
                    state
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("invalid state path"))?,
                    "--name",
                    &name,
                ])
                .stdin(std::process::Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .spawn()?;
            for _ in 0..100 {
                if let Ok(result) =
                    whatsai_core::daemon::request(&state, json!({"action":"health"})).await
                {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                    return Ok(());
                }
                if let Some(status) = child.try_wait()? {
                    // A concurrent `start` may have won the state lock; keep waiting for its daemon.
                    let log = std::fs::read(&log_path).unwrap_or_default();
                    let since_spawn =
                        String::from_utf8_lossy(&log[log.len().min(written_before as usize)..]);
                    if since_spawn.contains("another process owns this state directory") {
                        continue;
                    }
                    anyhow::bail!(
                        "daemon exited {status}; inspect {}",
                        state.join("daemon.log").display()
                    );
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            anyhow::bail!(
                "daemon did not become ready; inspect {}",
                state.join("daemon.log").display()
            );
        }
        Command::Files => json!({"action":"files"}),
        Command::AcceptHandoff {
            event,
            repo,
            directory,
        } => {
            json!({"action":"accept-handoff","event":event,"repo":std::fs::canonicalize(repo)?,"directory":if directory.is_absolute(){directory}else{std::env::current_dir()?.join(directory)}})
        }
        Command::Agents => json!({"action":"agents"}),
        Command::Agent { command } => match command {
            AgentCommand::Attach {
                harness,
                workspace,
                session,
                pid,
            } => {
                json!({"action":"agent","operation":"attach","harness":harness,"workspace":std::fs::canonicalize(workspace)?,"session":session,"pid":pid})
            }
            AgentCommand::Heartbeat { lease } => {
                json!({"action":"agent","operation":"heartbeat","lease":lease})
            }
            AgentCommand::Detach { lease } => {
                json!({"action":"agent","operation":"detach","lease":lease})
            }
            AgentCommand::Show { agent } => {
                json!({"action":"agent","operation":"show","agent":agent})
            }
            AgentCommand::Publish { agent } => {
                json!({"action":"agent","operation":"publish","agent":agent})
            }
            AgentCommand::Unpublish { agent } => {
                json!({"action":"agent","operation":"unpublish","agent":agent})
            }
            AgentCommand::Enroll { agent, into } => {
                json!({"action":"agent","operation":"enroll","agent":agent,"team":into})
            }
            AgentCommand::Unenroll { agent } => {
                json!({"action":"agent","operation":"unenroll","agent":agent})
            }
            AgentCommand::AutoEnroll { mode } => {
                json!({"action":"agent","operation":"auto-enroll","mode":mode})
            }
            AgentCommand::AutoPublish { mode } => {
                json!({"action":"agent","operation":"auto-publish","mode":mode})
            }
            AgentCommand::Retire { agent } => {
                json!({"action":"agent","operation":"retire","agent":agent})
            }
            AgentCommand::Adopt { agent, workspace } => {
                json!({"action":"agent","operation":"adopt","agent":agent,"workspace":std::fs::canonicalize(workspace)?})
            }
            AgentCommand::Unread {
                agent,
                harness,
                workspace,
            } => {
                json!({"action":"agent","operation":"unread","agent":agent,"harness":harness,"workspace":std::fs::canonicalize(workspace)?})
            }
            AgentCommand::MarkRead { agent } => {
                json!({"action":"agent","operation":"mark-read","agent":agent})
            }
        },
        Command::Worker { command } => match command {
            WorkerCommand::Bind {
                agent,
                adapter,
                harness,
                cwd,
                default,
            } => {
                json!({"action":"worker","operation":"bind","agent":agent,"harness":harness,"cwd":cwd.map(std::fs::canonicalize).transpose()?,"adapter":std::fs::canonicalize(adapter)?,"default":default})
            }
            WorkerCommand::Enable { agent } => {
                json!({"action":"worker","operation":"enable","agent":agent})
            }
            WorkerCommand::Pause { agent } => {
                json!({"action":"worker","operation":"pause","agent":agent})
            }
            WorkerCommand::Unbind { agent } => {
                json!({"action":"worker","operation":"unbind","agent":agent})
            }
            WorkerCommand::Status => json!({"action":"worker","operation":"status"}),
            WorkerCommand::Reset { agent, root } => {
                json!({"action":"worker","operation":"reset","agent":agent,"root":root})
            }
        },
        Command::Ui => {
            let state_for_ui = state.clone();
            return tokio::task::spawn_blocking(move || ui::main(state_for_ui)).await?;
        }
        Command::Version => {
            let mut report = json!({"cli":env!("CARGO_PKG_VERSION")});
            match whatsai_core::daemon::request(&state, json!({"action":"version"})).await {
                Ok(daemon) => {
                    report["daemon"] = daemon;
                    if report["daemon"]["daemon"] != env!("CARGO_PKG_VERSION") {
                        report["notice"] = json!(
                            "the running daemon is a different release; run `whatsai stop` then `whatsai start` after installing"
                        );
                    }
                }
                Err(e) => report["daemon"] = json!({"error":e.to_string()}),
            }
            if args.table {
                println!("{}", view::version(&report).join("\n"));
            } else {
                println!("{}", serde_json::to_string_pretty(&report)?);
            }
            return Ok(());
        }
        Command::Profiles => {
            println!(
                "{}",
                serde_json::to_string_pretty(&whatsai_core::storage::profiles())?
            );
            return Ok(());
        }
        Command::Teams => json!({"action":"teams"}),
        Command::Register => json!({"action":"register"}),
        Command::Health => json!({"action":"health"}),
        Command::Stop => json!({"action":"stop"}),
        Command::Sync => json!({"action":"sync"}),
        Command::Invite => json!({"action":"invite"}),
        Command::List => json!({"action":"list"}),
        Command::Requests => json!({"action":"requests"}),
        Command::Inbox {
            agent,
            harness,
            unread,
        } => json!({"action":"inbox","agent":agent,"harness":harness,"unread":unread}),
        Command::Outbox => json!({"action":"outbox"}),
        Command::Create {
            repository,
            workspace,
        } => {
            json!({"action":"create","repository":repository,"workspace":std::fs::canonicalize(workspace)?})
        }
        Command::Join { key, workspace } => {
            json!({"action":"join","key":key,"workspace":std::fs::canonicalize(workspace)?})
        }
        Command::JoinStatus => json!({"action":"join-status"}),
        Command::Approve { member } => json!({"action":"approve","member":member}),
        Command::Reject { member } => json!({"action":"reject","member":member}),
        Command::Promote { member } => json!({"action":"promote","member":member}),
        Command::Demote { member } => json!({"action":"demote","member":member}),
        Command::Revoke { member } => json!({"action":"revoke","member":member}),
        Command::Leave => json!({"action":"leave"}),
        Command::Send {
            text,
            to,
            to_agent,
            reply_to,
            agent,
            as_agent,
        } => {
            json!({"action":if agent||as_agent.is_some(){"agent-send"}else{"send"},"text":text,"to":to,"to_agent":to_agent,"reply_to":reply_to,"agent":as_agent})
        }
        Command::Share {
            path,
            agent,
            as_agent,
        } => {
            json!({"action":"share","path":std::fs::canonicalize(path)?,"actor":if agent||as_agent.is_some(){"agent"}else{"person"},"agent":as_agent})
        }
        Command::Download {
            file,
            directory,
            max_chunks,
        } => {
            json!({"action":"download","file":file,"directory":std::fs::canonicalize(directory)?,"max_chunks":max_chunks})
        }
        Command::Status {
            work_state,
            description,
            branch,
            commit,
            as_agent,
        } => {
            json!({"action":"status","state":work_state,"description":description,"branch":branch,"commit":commit,"actor":if as_agent.is_some(){"agent"}else{"person"},"agent":as_agent})
        }
        Command::Handoff {
            branch,
            commit,
            description,
            as_agent,
        } => {
            json!({"action":"handoff","branch":branch,"commit":commit,"description":description,"actor":if as_agent.is_some(){"agent"}else{"person"},"agent":as_agent})
        }
        Command::Rpc => {
            use std::io::Read;
            let mut bytes = vec![];
            std::io::stdin()
                .take(whatsai_core::protocol::MAX_FRAME as u64 + 1)
                .read_to_end(&mut bytes)?;
            anyhow::ensure!(
                bytes.len() <= whatsai_core::protocol::MAX_FRAME,
                "request too large"
            );
            serde_json::from_slice::<Value>(&bytes)?
        }
    };
    let mut command = command;
    if let Some(team) = &args.team {
        command["team"] = json!(team);
    }
    if let Ok(cwd) = std::env::current_dir() {
        command["cwd"] = json!(cwd);
    }
    let action = command["action"].as_str().unwrap_or("").to_owned();
    let result = whatsai_core::daemon::request(&state, command.clone()).await?;
    if args.table {
        // Inbox rows name senders when the team roster is at hand.
        let names = if matches!(action.as_str(), "inbox" | "files") {
            let mut list = json!({"action":"list"});
            for key in ["team", "cwd"] {
                if !command[key].is_null() {
                    list[key] = command[key].clone();
                }
            }
            whatsai_core::daemon::request(&state, list)
                .await
                .map(|t| t["members"].clone())
                .unwrap_or(json!({}))
        } else {
            json!({})
        };
        match view::render(&action, &result, &names) {
            Some(lines) => println!("{}", lines.join("\n")),
            None => println!("{}", serde_json::to_string_pretty(&result)?),
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&result)?);
    }
    Ok(())
}

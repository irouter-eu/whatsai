use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::path::PathBuf;
#[derive(Parser)]
#[command(version, about = "Private teamwork for people and their coding agents")]
struct Args {
    /// State directory; defaults to ~/.local/share/whatsai/<harness>.
    #[arg(long, env = "WHATSAI_STATE", global = true)]
    state: Option<PathBuf>,
    /// Which coding agent this command acts for (claude, codex, ...); picks the default state directory.
    #[arg(long, env = "WHATSAI_HARNESS", global = true)]
    harness: Option<String>,
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
    Stop,
    Sync,
    Invite,
    List,
    Requests,
    Inbox,
    Outbox,
    Files,
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
    /// Create a team; this daemon becomes the network's authority.
    Create {
        #[arg(long)]
        repository: String,
    },
    /// Request admission with a join key from an existing member.
    Join {
        key: String,
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
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        reply_to: Option<String>,
        #[arg(long)]
        agent: bool,
    },
    Share {
        path: PathBuf,
        #[arg(long)]
        agent: bool,
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
    },
    Handoff {
        #[arg(long)]
        branch: String,
        #[arg(long)]
        commit: String,
        #[arg(long)]
        description: Option<String>,
    },
    /// Structured local adapter API; request must be passed on standard input.
    Rpc,
}
#[derive(Subcommand)]
enum WorkerCommand {
    Bind {
        #[arg(long)]
        harness: String,
        #[arg(long)]
        cwd: PathBuf,
        #[arg(long)]
        adapter: PathBuf,
    },
    Enable,
    Pause,
    Status,
    Reset {
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
        None => whatsai_core::storage::default_state_for(args.harness.as_deref())?,
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
        Command::Worker { command } => match command {
            WorkerCommand::Bind {
                harness,
                cwd,
                adapter,
            } => {
                json!({"action":"worker","operation":"bind","harness":harness,"cwd":std::fs::canonicalize(cwd)?,"adapter":std::fs::canonicalize(adapter)?})
            }
            WorkerCommand::Enable => json!({"action":"worker","operation":"enable"}),
            WorkerCommand::Pause => json!({"action":"worker","operation":"pause"}),
            WorkerCommand::Status => json!({"action":"worker","operation":"status"}),
            WorkerCommand::Reset { root } => {
                json!({"action":"worker","operation":"reset","root":root})
            }
        },
        Command::Register => json!({"action":"register"}),
        Command::Health => json!({"action":"health"}),
        Command::Stop => json!({"action":"stop"}),
        Command::Sync => json!({"action":"sync"}),
        Command::Invite => json!({"action":"invite"}),
        Command::List => json!({"action":"list"}),
        Command::Requests => json!({"action":"requests"}),
        Command::Inbox => json!({"action":"inbox"}),
        Command::Outbox => json!({"action":"outbox"}),
        Command::Create { repository } => json!({"action":"create","repository":repository}),
        Command::Join { key } => json!({"action":"join","key":key}),
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
            reply_to,
            agent,
        } => {
            json!({"action":if agent{"agent-send"}else{"send"},"text":text,"to":to,"reply_to":reply_to})
        }
        Command::Share { path, agent } => {
            json!({"action":"share","path":std::fs::canonicalize(path)?,"actor":if agent{"agent"}else{"person"}})
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
        } => {
            json!({"action":"status","state":work_state,"description":description,"branch":branch,"commit":commit})
        }
        Command::Handoff {
            branch,
            commit,
            description,
        } => json!({"action":"handoff","branch":branch,"commit":commit,"description":description}),
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
    println!(
        "{}",
        serde_json::to_string_pretty(&whatsai_core::daemon::request(&state, command).await?)?
    );
    Ok(())
}

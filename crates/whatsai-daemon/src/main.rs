use clap::Parser;
#[derive(Parser)]
#[command(version, about = "WhatsAI local inbox and networking daemon")]
struct Args {
    #[arg(long, env = "WHATSAI_STATE")]
    state: Option<std::path::PathBuf>,
    #[arg(long, env = "WHATSAI_HARNESS")]
    harness: Option<String>,
    #[arg(long, default_value = "Member")]
    name: String,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let state = match args.state {
        Some(state) => state,
        None => whatsai_core::storage::default_state_for(args.harness.as_deref())?,
    };
    whatsai_core::daemon::run(&state, &args.name).await
}

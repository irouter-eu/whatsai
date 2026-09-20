use clap::Parser;
#[derive(Parser)]
#[command(version, about = "WhatsAI local inbox and networking daemon")]
struct Args {
    #[arg(long, env = "WHATSAI_STATE")]
    state: Option<std::path::PathBuf>,
    #[arg(long, default_value = "Member")]
    name: String,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let state = args
        .state
        .unwrap_or_else(whatsai_core::storage::default_state);
    whatsai_core::daemon::run(&state, &args.name).await
}

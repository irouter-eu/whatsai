use clap::Parser;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
#[derive(Parser)]
#[command(version, about = "WhatsAI membership authority and encrypted mailbox")]
struct Args {
    #[arg(long)]
    state: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    #[arg(long,default_value_t=1024*1024*1024)]
    quota_bytes: usize,
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let _lock = whatsai_core::storage::lock(&args.state)?;
    let service = Arc::new(whatsai_core::service::Service::open(
        &args.state,
        args.quota_bytes,
    )?);
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!("WhatsAI authority listening on {}", listener.local_addr()?);
    axum_serve(listener, service).await
}
async fn axum_serve(
    listener: tokio::net::TcpListener,
    service: Arc<whatsai_core::service::Service>,
) -> anyhow::Result<()> {
    whatsai_core::service::serve(listener, service).await
}

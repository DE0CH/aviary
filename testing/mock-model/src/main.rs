//! A scripted Anthropic Messages API and models route for tests (platform 7.5). See the library docs.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "A scripted Anthropic Messages API and models route for tests")]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:80")]
    listen: SocketAddr,
    /// The models route's answer (gateway-api's `Models` JSON); default: gateway-api's fixture.
    #[arg(long)]
    models: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mock = match &args.models {
        Some(p) => {
            let text = std::fs::read_to_string(p).with_context(|| p.display().to_string())?;
            mock_model::MockModel::new(serde_json::from_str(&text).context("models")?)
        }
        None => mock_model::MockModel::with_default_models(),
    };
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!("mock-model listening on {}", listener.local_addr()?);
    axum::serve(listener, mock.router()).await?;
    Ok(())
}

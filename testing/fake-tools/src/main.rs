//! A minimal MCP server for tests (platform 7.6). See the library docs.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "A minimal MCP server for tests")]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:80")]
    listen: SocketAddr,
    /// A file holding the gateway's tools pair token (the bearer every `/mcp` request must carry).
    #[arg(long)]
    token_file: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let token = std::fs::read_to_string(&args.token_file).with_context(|| args.token_file.display().to_string())?;
    let token = token.trim();
    anyhow::ensure!(!token.is_empty(), "{}: empty token", args.token_file.display());
    let tools = fake_tools::FakeTools::new(token);
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!("fake-tools listening on {}", listener.local_addr()?);
    axum::serve(listener, tools.router()).await?;
    Ok(())
}

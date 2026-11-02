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
    /// A file holding the gateway's tools pair tokens, one per line (the bearers `/mcp` accepts: the current token
    /// and, during a rotation, the next one), read again on every request.
    #[arg(long)]
    token_file: PathBuf,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let tools = fake_tools::FakeTools::from_token_file(&args.token_file)
        .with_context(|| args.token_file.display().to_string())?;
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!("fake-tools listening on {}", listener.local_addr()?);
    axum::serve(listener, tools.router()).await?;
    Ok(())
}

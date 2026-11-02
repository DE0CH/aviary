//! A Cloudflare Access issuer for tests: JWKS, get-identity, token minting (platform 7.4). See the library docs.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "A Cloudflare Access issuer for tests")]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:443")]
    listen: SocketAddr,
    /// The team domain (`iss` = `https://<team domain>`).
    #[arg(long)]
    team_domain: String,
    /// A signing key as `<kid>=<path of a PKCS#8 PEM RSA key>`; repeat for a second key (the first signs by
    /// default).
    #[arg(long = "key", required = true)]
    keys: Vec<String>,
    /// TLS certificate chain (PEM); without it the issuer speaks plain HTTP.
    #[arg(long, requires = "tls_key")]
    tls_cert: Option<PathBuf>,
    /// TLS key (PKCS#8 PEM).
    #[arg(long, requires = "tls_cert")]
    tls_key: Option<PathBuf>,
}

fn read(p: &PathBuf) -> anyhow::Result<String> {
    std::fs::read_to_string(p).with_context(|| p.display().to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut keys = Vec::new();
    for k in &args.keys {
        let (kid, path) = k.split_once('=').context("--key <kid>=<path>")?;
        keys.push(fake_access::Key::from_pem(kid, &read(&PathBuf::from(path))?)?);
    }
    let issuer = fake_access::Issuer::new(&args.team_domain, keys)?;
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    eprintln!("fake-access listening on {}", listener.local_addr()?);
    match (&args.tls_cert, &args.tls_key) {
        (Some(c), Some(k)) => fake_access::serve_tls(listener, issuer.router(), &read(c)?, &read(k)?).await,
        _ => Ok(axum::serve(listener, issuer.router()).await?),
    }
}

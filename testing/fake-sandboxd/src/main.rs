//! `fake-sandboxd`: sandboxd's API backed by Kubernetes Pods of the real sandbox image, and the EC2 calls hermit
//! makes (platform 7.2). See the library docs.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context as _;
use clap::{Args, Parser, Subcommand};
use fake_sandboxd::host::{Facts, HostFile};

#[derive(Parser, Debug)]
#[command(about = "sandboxd's API on Kubernetes Pods, and EC2's calls, for aviary's harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Args, Debug, Clone)]
struct FactsArgs {
    /// The namespace of the Pods.
    #[arg(long)]
    namespace: String,
    /// The node path of the `JuiceFS` mount.
    #[arg(long, default_value = "/mnt/aviary/jfs")]
    jfs_root: String,
    #[arg(long, default_value_t = 8)]
    cpus: u64,
    /// Bytes.
    #[arg(long, default_value_t = 16 << 30)]
    mem_total: u64,
    /// What `runsc cpu-features` would print (hermit.json `fabric.cpuFeatures`).
    #[arg(long, default_value = "")]
    cpu_features: String,
    /// What `runsc --version` would print.
    #[arg(long, default_value = "fake")]
    runsc: String,
    /// The runsc fork release (hermit.json `fabric.runsc`).
    #[arg(long, default_value = "fake")]
    gvisor_release: String,
    /// sandboxd's version (hermit.json `fabric.sandboxd`).
    #[arg(long, default_value = "fake")]
    sandboxd_version: String,
}

impl FactsArgs {
    fn facts(self) -> Facts {
        Facts {
            namespace: self.namespace,
            jfs_root: self.jfs_root,
            cpus: self.cpus,
            mem_total: self.mem_total,
            cpu_features: self.cpu_features,
            runsc: self.runsc,
            gvisor_release: self.gvisor_release,
            version: self.sandboxd_version,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Command {
    /// One fabric host: sandboxd's API on mutual TLS, sandboxes as Pods.
    Host {
        /// The host's config (hermit's user data shape).
        #[arg(long)]
        config: PathBuf,
        #[arg(long, default_value = "0.0.0.0:7443")]
        listen: SocketAddr,
        #[command(flatten)]
        facts: FactsArgs,
    },
    /// EC2's `RunInstances`, `DescribeInstances` and `TerminateInstances`, with host Pods as instances.
    Ec2 {
        #[arg(long, default_value = "0.0.0.0:443")]
        listen: SocketAddr,
        /// TLS certificate chain (PEM) for EC2's name.
        #[arg(long)]
        tls_cert: PathBuf,
        /// Its key (PKCS#8 PEM).
        #[arg(long)]
        tls_key: PathBuf,
        /// The image host Pods run (`fake-sandboxd` on its PATH).
        #[arg(long)]
        host_image: String,
        /// The service account of the host Pods.
        #[arg(long, default_value = "fake-sandboxd")]
        service_account: String,
        #[command(flatten)]
        facts: FactsArgs,
    },
}

fn read(p: &PathBuf) -> anyhow::Result<String> {
    std::fs::read_to_string(p).with_context(|| p.display().to_string())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    match Cli::parse().command {
        Command::Host { config, listen, facts } => {
            let file: HostFile = serde_json::from_str(&read(&config)?).context("host config")?;
            fake_sandboxd::host::run(file, facts.facts(), listen).await
        }
        Command::Ec2 {
            listen,
            tls_cert,
            tls_key,
            host_image,
            service_account,
            facts,
        } => {
            let id = fake_sandboxd::tls::Identity {
                cert_pem: read(&tls_cert)?,
                key_pem: read(&tls_key)?,
                client_ca_pem: None,
            };
            let opts = fake_sandboxd::ec2::Options {
                host_image,
                service_account,
                facts: facts.facts(),
            };
            fake_sandboxd::ec2::run(opts, listen, id).await
        }
    }
}

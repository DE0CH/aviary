//! `fabric-aws --config hermit.json <command>`: the AWS side of hermit's fabric (lib.rs). Credentials: the standard
//! `AWS_*` environment (or profile) of whoever runs it; the region is the config's. Every command is idempotent.
//! Exit status: 0 done; 1 failed (or `check`: the AMI must be baked); 2 usage; 3 `sync` refused (a sandboxd pin that is
//! no release can never match an AMI, so every run would bake again); 4 `check`: only the template's disk differs.

use anyhow::Result;
use clap::{Parser, Subcommand};
use fabric_aws::aws::Cli as AwsCli;
use fabric_aws::bake::{self, Bake, Ghcr};
use fabric_aws::cmds;
use fabric_aws::config::{Config, release_pin};
use fabric_aws::plan::{self, Verdict};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(about = "The AWS side of hermit's fabric, every value from the deployment's hermit.json")]
struct Args {
    /// the deployment's hermit.json
    #[arg(long)]
    config: PathBuf,
    /// a registry login for a private sandbox image (read:packages); a public one needs none
    #[arg(long, env = "GHCR_USER", hide_env_values = true)]
    ghcr_user: Option<String>,
    #[arg(long, env = "GHCR_TOKEN", hide_env_values = true)]
    ghcr_token: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args)]
struct BakeArgs {
    /// aviary's fabric/host (setup.sh, fetch-sandboxd.sh), copied to the builder
    #[arg(long)]
    host_dir: PathBuf,
}

#[derive(Subcommand)]
enum Cmd {
    /// The security group named after the template: `WireGuard` (udp 51820) from the VPC only; nothing else in
    Sg,
    /// IPv6 for the hosts: an Amazon /56 on the VPC, a /64 on the subnet, `::/0` to the internet gateway, IPv6 egress
    Ipv6,
    /// Bake an AMI on a temporary builder (a killed earlier bake's builder, key and SSH rule go first); prints its id
    Ami {
        /// the sandbox image (sandbox.image unless given)
        image: Option<String>,
        #[command(flatten)]
        bake: BakeArgs,
        /// the sandboxd release (fabric.sandboxd unless given)
        #[arg(long)]
        sandboxd_release: Option<String>,
        /// a local sandboxd build instead (its AMI is tagged sandboxd=local and never matches a pin)
        #[arg(long)]
        sandboxd_binary: Option<PathBuf>,
        /// the gVisor fork's release (fabric.runsc unless given)
        #[arg(long)]
        runsc: Option<String>,
    },
    /// The launch template's default AMI and its tags (image, sandboxd, runsc) and disk
    Current,
    /// Exit 0 when that AMI holds exactly the three pins and the disk is fabric.aws.diskGiB; 1 (bake) or 4 (disk) else
    Check,
    /// Make the template's AMI hold the pins: bake if `check` fails (or --force), then `template` and `prune-amis`
    Sync {
        #[command(flatten)]
        bake: BakeArgs,
        #[arg(long)]
        force: bool,
    },
    /// A new default launch template version with that AMI; the other versions are deleted
    Template { ami: String },
    /// IAM user hermit-scaler with only scaler-policy.json
    Iam,
    /// A new access key of hermit-scaler, written to --key-out (mode 600)
    IamKey {
        #[arg(long)]
        key_out: PathBuf,
    },
    /// IAM user hermit-ami-ci with the managed policy ami-ci-policy.json
    CiIam,
    /// A new access key of hermit-ami-ci, written to --key-out (mode 600)
    CiIamKey {
        #[arg(long)]
        key_out: PathBuf,
    },
    /// Deregister every AMI tagged app=<tags.app> but the template's (and their snapshots)
    PruneAmis,
    /// The hosts' image store: a private bucket, an S3 gateway endpoint on the hosts' route table, its policy
    ImageBucket,
    /// Put the sandbox image (sandbox.image unless given) in the bucket, unpacked as sandboxd keeps it
    PublishImage { image: Option<String> },
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "fabric-aws: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<u8> {
    let cfg = Config::load(&args.config)?;
    let aws = AwsCli {
        region: cfg.region.clone(),
    };
    let ghcr = match (&args.ghcr_user, &args.ghcr_token) {
        (Some(u), Some(t)) if !u.is_empty() && !t.is_empty() => Some(Ghcr {
            user: u.clone(),
            token: t.clone(),
        }),
        _ => None,
    };
    let mut out = std::io::stdout().lock();
    let mut log = std::io::stderr().lock();
    let bake_of = |host_dir: &PathBuf| Bake {
        image: cfg.pin_image.clone(),
        sandboxd_release: cfg.pin_sandboxd.clone(),
        sandboxd_binary: None,
        runsc: cfg.pin_runsc.clone(),
        host_dir: host_dir.clone(),
        ghcr: ghcr.as_ref(),
    };
    match &args.cmd {
        Cmd::Sg => cmds::sg(&aws, &cfg, &mut out)?,
        Cmd::Ipv6 => cmds::ipv6(&aws, &cfg, &mut out, &|s| {
            std::thread::sleep(std::time::Duration::from_secs(s));
        })?,
        Cmd::Ami {
            image,
            bake,
            sandboxd_release,
            sandboxd_binary,
            runsc,
        } => {
            let mut b = bake_of(&bake.host_dir);
            if let Some(i) = image {
                b.image.clone_from(i);
            }
            if let Some(r) = sandboxd_release {
                b.sandboxd_release.clone_from(r);
            }
            if let Some(r) = runsc {
                b.runsc.clone_from(r);
            }
            b.sandboxd_binary.clone_from(sandboxd_binary);
            bake::ami(&aws, &cfg, &b, &mut out, &mut log)?;
        }
        Cmd::Current => cmds::current(&aws, &cfg, &mut out)?,
        Cmd::Check => {
            let want = bake::digest_of(&cfg.pin_image, ghcr.as_ref())?;
            return report(&cmds::check(&aws, &cfg, &want)?, &mut out, &mut log);
        }
        Cmd::Sync { bake, force } => {
            if !release_pin(&cfg.pin_sandboxd) {
                writeln!(
                    log,
                    "sync refused: fabric.sandboxd '{}' is not an aviary-<sha12> release, so a baked AMI could never pass check",
                    cfg.pin_sandboxd
                )?;
                return Ok(3);
            }
            if !force {
                let want = bake::digest_of(&cfg.pin_image, ghcr.as_ref())?;
                match cmds::check(&aws, &cfg, &want)? {
                    v @ Verdict::Ok(_) => return report(&v, &mut out, &mut log),
                    Verdict::DiskOnly(why) => {
                        // the same AMI, a new template version
                        writeln!(log, "stale: {why}")?;
                        let (ami, _) = cmds::current_ami(&aws, &cfg)?;
                        cmds::template(&aws, &cfg, &ami, &mut out)?;
                        cmds::current(&aws, &cfg, &mut out)?;
                        return Ok(0);
                    }
                    Verdict::Stale(why) => writeln!(log, "stale: {why}")?,
                }
            }
            let new = bake::ami(&aws, &cfg, &bake_of(&bake.host_dir), &mut out, &mut log)?;
            cmds::template(&aws, &cfg, &new, &mut out)?;
            cmds::prune_amis(&aws, &cfg, &mut out)?;
            cmds::current(&aws, &cfg, &mut out)?;
        }
        Cmd::Template { ami } => cmds::template(&aws, &cfg, ami, &mut out)?,
        Cmd::Iam => cmds::iam(&aws, &cfg, &mut out)?,
        Cmd::IamKey { key_out } => cmds::iam_key(&aws, plan::SCALER_USER, key_out, &mut out)?,
        Cmd::CiIam => cmds::ci_iam(&aws, &cfg, &mut out)?,
        Cmd::CiIamKey { key_out } => cmds::iam_key(&aws, plan::CI_USER, key_out, &mut out)?,
        Cmd::PruneAmis => cmds::prune_amis(&aws, &cfg, &mut out)?,
        Cmd::ImageBucket => cmds::image_bucket(&aws, &cfg, &mut out)?,
        Cmd::PublishImage { image } => {
            bake::publish_image(
                &aws,
                &cfg,
                image.as_deref().unwrap_or(&cfg.pin_image),
                ghcr.as_ref(),
                &mut out,
            )?;
        }
    }
    Ok(0)
}

fn report(v: &Verdict, out: &mut dyn Write, log: &mut dyn Write) -> Result<u8> {
    match v {
        Verdict::Ok(m) => writeln!(out, "ok: {m}")?,
        Verdict::Stale(m) | Verdict::DiskOnly(m) => writeln!(log, "stale: {m}")?,
    }
    Ok(v.code())
}

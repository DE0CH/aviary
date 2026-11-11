//! `aviary-gateway`: an aviary session machine's half of the sandbox gateway (src/machine.rs), started by
//! session-image/base/harness-aviary.sh as the session user.
//!
//! Env: `AVIARY_GW_URL`, `AVIARY_GW_ACCESS_ID`, `AVIARY_GW_ACCESS_SECRET` (store `aviary`), `SESSION_ID`,
//! `SESSION_MODEL` (`aviary/<Claude model id>`), `AVIARY_IMAGE` (`sha256:…`), `AVIARY_BOOT_FILE`; the descriptor's
//! Jarvis values from the machine's own env (src/wire.rs `BOOT_ENV_NAMES`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use aviary_core::{env, log, version};
use jarvis_aviary::machine::{self, Config};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let build = version::Build::new("aviary-gateway");
    log::init_in_pod(&build, log::Output::Stdout);
    let model = env::required("SESSION_MODEL");
    let cfg = Config {
        gateway_url: env::required("AVIARY_GW_URL")
            .trim_end_matches('/')
            .to_owned(),
        access_id: env::required("AVIARY_GW_ACCESS_ID"),
        access_secret: env::required("AVIARY_GW_ACCESS_SECRET"),
        session: env::required("SESSION_ID"),
        model: model.strip_prefix("aviary/").unwrap_or(&model).to_owned(),
        credentials: PathBuf::from(env::required("HOME")).join(".claude/.credentials.json"),
        boot_file: PathBuf::from(env::required("AVIARY_BOOT_FILE")),
        image: env::required("AVIARY_IMAGE"),
        env: jarvis_aviary::wire::BOOT_ENV_NAMES
            .iter()
            .filter_map(|k| {
                Some((
                    (*k).to_owned(),
                    std::env::var(k).ok().filter(|v| !v.is_empty())?,
                ))
            })
            .collect::<BTreeMap<_, _>>(),
    };
    machine::run(cfg).await
}

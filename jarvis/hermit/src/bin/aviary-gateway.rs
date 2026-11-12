//! `aviary-gateway`: an aviary session machine's half of the sandbox gateway (src/machine.rs), started by
//! session-image/base/harness-aviary.sh as the session user.
//!
//! Env: `AVIARY_GW_URL`, `AVIARY_GW_ACCESS_ID`, `AVIARY_GW_ACCESS_SECRET` (store `aviary-gw`), `SESSION_ID`,
//! `SESSION_MODEL` (`aviary/<model id>`), `OPENROUTER_API` (store `openrouter`: an OpenRouter model's calls),
//! `AVIARY_IMAGE` (`sha256:…`), `AVIARY_BOOT_FILE`; the descriptor's
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
        // the e2e's stand-in model (a key of its `aviary-gw` store); Anthropic otherwise
        model_url: std::env::var("AVIARY_MODEL_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .map_or_else(
                || machine::ANTHROPIC.to_owned(),
                |v| v.trim_end_matches('/').to_owned(),
            ),
        // the e2e's stand-in for OpenRouter; OpenRouter otherwise
        openrouter_url: std::env::var("AVIARY_OPENROUTER_URL")
            .ok()
            .filter(|v| !v.is_empty())
            .map_or_else(
                || machine::OPENROUTER.to_owned(),
                |v| v.trim_end_matches('/').to_owned(),
            ),
        openrouter_key: std::env::var("OPENROUTER_API")
            .ok()
            .filter(|v| !v.is_empty()),
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

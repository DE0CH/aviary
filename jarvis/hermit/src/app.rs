//! hermit-jarvis's shared state and configuration (every value from the environment, aviary_core::env).

use std::sync::Arc;
use std::time::Duration;

use aviary_core::env;
use aviary_pg::Pool;
use hermit_api::token::{ControlKey, KeySet, MasterKey, SigningKey, TokenKeys};

use crate::jarvis::Jarvis;

/// How long a sandbox token lasts: longer than any machine runs (an idle session pauses within hours; each boot gets
/// a new sandbox and token).
pub const LIFETIME: Duration = Duration::from_secs(30 * 24 * 3600);

/// A resume that has not booted the machine in this long ends the waiting sandbox (`start_timed_out`).
pub const RESUME_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// The models these sessions offer (the session's model is picked in the Jarvis 4 app; the list is what pigeon shows).
pub const MODELS: &[(&str, &str)] = &[
    ("claude-opus-5-5", "Opus 5.5"),
    ("claude-fable-5-1", "Fable 5.1"),
];
/// Claude Code's small model and the title model.
pub const SMALL_MODEL: &str = "claude-haiku-5-5";

/// Everything a request needs.
#[derive(Clone, Debug)]
pub struct App(pub Arc<Inner>);

#[derive(Debug)]
pub struct Inner {
    pub pool: Pool,
    pub keys: Keys,
    pub magpie: magpie_api::client::Client,
    pub jarvis: Jarvis,
    /// Deyao's email: the only person, an admin.
    pub owner: String,
    /// The Access application pigeon accepts (chat.deyaochen.com).
    pub access_team: String,
    pub access_aud: String,
    /// Where the gateway listener forwards the fixed names.
    pub magpie_url: String,
    pub hermit_url: String,
    /// The gateway's pair tokens to magpie and to this hermit.
    pub gateway_to_magpie: String,
    pub gateway_to_hermit: String,
    pub http: reqwest::Client,
}

impl std::ops::Deref for App {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// The token keys: every master key's public key is published, the first signs new tokens.
pub struct Keys {
    pub signing: SigningKey,
    pub control: Vec<(u32, ControlKey)>,
    pub published: TokenKeys,
    pub verify: KeySet,
}

impl std::fmt::Debug for Keys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keys")
            .field("published", &self.published)
            .finish_non_exhaustive()
    }
}

impl Keys {
    /// From `HERMIT_TOKEN_KEYS` (`kid:base64url`, comma-separated, the signing key first), as YYC's hermit reads it.
    ///
    /// # Errors
    /// A malformed list or a key that cannot be derived.
    pub fn parse(list: &str) -> anyhow::Result<Self> {
        let masters =
            MasterKey::parse_list(list).map_err(|e| anyhow::anyhow!("HERMIT_TOKEN_KEYS: {e}"))?;
        let first = masters
            .first()
            .ok_or_else(|| anyhow::anyhow!("HERMIT_TOKEN_KEYS is empty"))?;
        let signing = first.signing_key().map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut control = Vec::new();
        let mut keys = Vec::new();
        for m in &masters {
            control.push((m.kid, m.control_key().map_err(|e| anyhow::anyhow!("{e}"))?));
            keys.push(
                m.signing_key()
                    .map_err(|e| anyhow::anyhow!("{e}"))?
                    .public_key(),
            );
        }
        let published = TokenKeys { keys };
        let verify = KeySet::from_keys(&published);
        Ok(Self {
            signing,
            control,
            published,
            verify,
        })
    }

    /// The control token hermit hands the machine for a sandbox (the signing kid's).
    #[must_use]
    pub fn control_token(&self, sandbox: uuid::Uuid) -> String {
        self.control
            .first()
            .map(|(_, k)| k.control_token(sandbox))
            .unwrap_or_default()
    }

    /// The descriptor's `control.tokenSha256`: every kid's token for the sandbox.
    #[must_use]
    pub fn control_hashes(&self, sandbox: uuid::Uuid) -> Vec<String> {
        self.control
            .iter()
            .map(|(_, k)| hermit_api::token::sha256_hex(&k.control_token(sandbox)))
            .collect()
    }
}

/// The service's environment.
#[derive(Debug)]
pub struct Env {
    pub database_url: String,
    pub token_keys: String,
    pub jarvis_url: String,
    pub jarvis_access_id: String,
    pub jarvis_access_secret: String,
    pub owner: String,
    pub access_team: String,
    pub access_aud: String,
}

impl Env {
    /// Every variable is required (a missing or empty one aborts the start).
    #[must_use]
    pub fn read() -> Self {
        Self {
            database_url: env::required("HERMIT_DATABASE_URL"),
            token_keys: env::required("HERMIT_TOKEN_KEYS"),
            jarvis_url: env::required("HERMIT_JARVIS_URL"),
            jarvis_access_id: env::required("HERMIT_JARVIS_ACCESS_ID"),
            jarvis_access_secret: env::required("HERMIT_JARVIS_ACCESS_SECRET"),
            owner: env::required("HERMIT_OWNER").trim().to_lowercase(),
            access_team: env::required("HERMIT_ACCESS_TEAM"),
            access_aud: env::required("HERMIT_ACCESS_AUD"),
        }
    }
}

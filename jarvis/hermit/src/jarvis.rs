//! hermit-jarvis's calls to Jarvis 4's router, with its own Access service token (admitted by client id on the "Jarvis 4 devices"
//! app, router/devices.go): whose machine signed a boot, which aviary sessions exist and in what state, and resuming a
//! paused one when its conversation gets a turn. hermit-jarvis never starts a new session (Deyao, 2026-11-11: sessions
//! start only in the Jarvis 4 app).

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The harness name of these sessions in Jarvis 4.
pub const HARNESS: &str = "aviary";

/// The router's base URL and the device's Access service token.
#[derive(Clone)]
pub struct Jarvis {
    base: String,
    id: String,
    secret: String,
    http: reqwest::Client,
}

impl std::fmt::Debug for Jarvis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Jarvis")
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// `POST /api/machine-signatures/verify` body: a request a session machine signed, as the router's `/m` checks it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyRequest {
    pub machine: String,
    pub method: String,
    pub path: String,
    /// Unix seconds, as signed.
    pub time: String,
    /// The sha256 hex of the body, as signed.
    pub body_sha256: String,
    pub sig: String,
}

/// Its answer: the machine's session as the router knows it (403 for a signature that does not verify).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Verified {
    pub session: String,
    pub harness: String,
    pub state: String,
}

/// One session of `GET /api/remotes?harness=aviary`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Remote {
    pub id: String,
    pub state: String,
    #[serde(default)]
    pub model: String,
}

#[derive(Deserialize)]
struct Remotes {
    sessions: Vec<Remote>,
}

/// A session state in which its machine is (or is about to be) up.
#[must_use]
pub fn is_up(state: &str) -> bool {
    matches!(state, "started" | "starting" | "resuming")
}

impl Jarvis {
    /// # Errors
    /// When the HTTP client cannot be built.
    pub fn new(base: &str, id: &str, secret: &str) -> anyhow::Result<Self> {
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            id: id.to_owned(),
            secret: secret.to_owned(),
            http: reqwest::Client::builder()
                .tls_backend_preconfigured(aviary_core::http::tls_config())
                .timeout(Duration::from_secs(20))
                .user_agent(concat!("hermit-jarvis/", env!("CARGO_PKG_VERSION")))
                .build()?,
        })
    }

    fn req(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, format!("{}{path}", self.base))
            .header("CF-Access-Client-Id", &self.id)
            .header("CF-Access-Client-Secret", &self.secret)
    }

    /// Whose machine signed this. `Ok(None)` when the router says it does not verify.
    ///
    /// # Errors
    /// The router unreachable or answering anything else.
    pub async fn verify(&self, v: &VerifyRequest) -> anyhow::Result<Option<Verified>> {
        let r = self
            .req(reqwest::Method::POST, "/api/machine-signatures/verify")
            .json(v)
            .send()
            .await?;
        match r.status().as_u16() {
            200 => Ok(Some(r.json().await?)),
            403 | 404 => Ok(None),
            s => anyhow::bail!("router answered {s} to a signature check"),
        }
    }

    /// Every aviary session the router lists.
    ///
    /// # Errors
    /// The router unreachable or answering anything but 200.
    pub async fn sessions(&self) -> anyhow::Result<Vec<Remote>> {
        let r = self
            .req(
                reqwest::Method::GET,
                &format!("/api/remotes?harness={HARNESS}"),
            )
            .send()
            .await?;
        if !r.status().is_success() {
            anyhow::bail!("router answered {} to the session list", r.status());
        }
        Ok(r.json::<Remotes>().await?.sessions)
    }

    /// Resumes a paused session (no prompt, no Face ID, as the Jarvis 4 Chat app did).
    ///
    /// # Errors
    /// The router unreachable or refusing.
    pub async fn resume(&self, session: &str) -> anyhow::Result<()> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/api/sessions/{session}/start"),
            )
            .json(&serde_json::json!({}))
            .send()
            .await?;
        if !r.status().is_success() {
            let s = r.status();
            let body = r.text().await.unwrap_or_default();
            anyhow::bail!(
                "router answered {s} to a resume: {}",
                body.chars().take(300).collect::<String>()
            );
        }
        Ok(())
    }
}

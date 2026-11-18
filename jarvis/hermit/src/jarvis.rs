//! hermit-jarvis's calls to Jarvis 4's router, with its own Access service token (admitted by client id on the "Jarvis 4 devices"
//! app, router/devices.go): whose machine signed a boot, which aviary sessions exist and in what state, resuming a
//! paused one when its conversation gets a turn, and proposing a new one for a conversation begun in the chat (Deyao,
//! 2026-11-18), which starts only once Deyao approves it in the Inbox as any New session.

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
    /// The session's Claude account (`claude-<name>`; empty: none, or a router from before accounts were listed).
    #[serde(default)]
    pub account: String,
}

/// One Claude account the chat's menu offers (`GET /api/remotes` `accounts`, the router's signed-in ones, the default
/// first).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Account {
    /// `claude-<name>`.
    pub store: String,
    /// The store without `claude-`: what the menu shows and an entry's id names.
    pub name: String,
}

/// `GET /api/remotes?harness=aviary`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Listing {
    pub sessions: Vec<Remote>,
    /// Absent from a router from before the chat offered accounts: none, and the menu is the file's.
    #[serde(default)]
    pub accounts: Vec<Account>,
}

impl Listing {
    /// The names of the accounts, `first` (the session's own, `claude-<name>` or its name) moved to the front.
    #[must_use]
    pub fn account_names(&self, first: Option<&str>) -> Vec<String> {
        let first = first.map(|f| f.strip_prefix("claude-").unwrap_or(f));
        let mut names: Vec<String> = self.accounts.iter().map(|a| a.name.clone()).collect();
        if let Some(f) = first
            && let Some(i) = names.iter().position(|n| n == f)
        {
            let n = names.remove(i);
            names.insert(0, n);
        }
        names
    }
}

/// The router's word on a switch of a session's Claude account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Switch {
    /// The session is on it already: nothing changes on its machine.
    Unchanged,
    /// The session's account changed; its machine is getting the new account's login.
    Switched,
    /// Refused for good (an account the router does not have, one with no login): its words.
    Refused(String),
    /// Not now (another change of the session runs): its words.
    Busy(String),
}

/// The router's state of a new session waiting for Deyao's approval.
pub const APPROVAL: &str = "approval";

/// The router's state of a fabric session waiting in the time machine (jarvis4 router/timemachine.go): its machine,
/// supervisor and all, is frozen as it was, and anything that would resume a paused session thaws it.
pub const FROZEN: &str = "frozen";

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
        Ok(self.listing().await?.sessions)
    }

    /// Every aviary session the router lists, and the Claude accounts the chat's menu offers.
    ///
    /// # Errors
    /// The router unreachable or answering anything but 200.
    pub async fn listing(&self) -> anyhow::Result<Listing> {
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
        Ok(r.json::<Listing>().await?)
    }

    /// Switches `session` to the Claude account `name` (`POST /api/remotes/{id}/account`: the chat's menu picked it),
    /// as the app's More → Switch Claude account does: the router answers at once, the machine gets the login after.
    ///
    /// # Errors
    /// The router unreachable or failing (5xx).
    pub async fn switch_account(&self, session: &str, name: &str) -> anyhow::Result<Switch> {
        let r = self
            .req(
                reqwest::Method::POST,
                &format!("/api/remotes/{session}/account"),
            )
            .json(&serde_json::json!({"account": name}))
            .send()
            .await?;
        let status = r.status();
        let body: serde_json::Value = r.json().await.unwrap_or_default();
        let words = || {
            body.get("error")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("the router refused the switch")
                .to_owned()
        };
        Ok(match status.as_u16() {
            200 if body.get("unchanged") == Some(&serde_json::Value::Bool(true)) => {
                Switch::Unchanged
            }
            200 | 202 => Switch::Switched,
            409 => Switch::Busy(words()),
            s if s < 500 => Switch::Refused(words()),
            s => anyhow::bail!("router answered {s} to an account switch: {}", words()),
        })
    }

    /// Tells the router when the soonest of a session's chat schedules fires next (`None`: none armed). An aviary
    /// session's wakeups are magpie's schedules (Deyao, 2026-11-17); the router counts one as armed (a paused session
    /// keeps its runners) and never fires it: magpie does, and [`Self::resume`] wakes the session for the turn.
    ///
    /// # Errors
    /// The router unreachable or refusing.
    pub async fn set_chat_schedule(
        &self,
        session: &str,
        next_at: Option<time::OffsetDateTime>,
    ) -> anyhow::Result<()> {
        let next = next_at
            .map(|t| t.format(&time::format_description::well_known::Rfc3339))
            .transpose()?;
        let r = self
            .req(
                reqwest::Method::PUT,
                &format!("/api/sessions/{session}/chat-schedule"),
            )
            .json(&serde_json::json!({ "nextAt": next }))
            .send()
            .await?;
        if !r.status().is_success() {
            anyhow::bail!("router answered {} to a chat schedule", r.status());
        }
        Ok(())
    }

    /// Proposes a new aviary session with the defaults (`POST /api/remotes`), idempotent by `request_id` while the
    /// session exists: `Ok(Ok(session))` with its id and state (`approval` until Deyao approves it), `Ok(Err(why))`
    /// when the router refuses it.
    ///
    /// # Errors
    /// The router unreachable or failing (5xx).
    pub async fn propose(&self, request_id: &str) -> anyhow::Result<Result<Remote, String>> {
        let r = self
            .req(reqwest::Method::POST, "/api/remotes")
            .json(&serde_json::json!({ "harness": HARNESS, "requestId": request_id }))
            .send()
            .await?;
        let s = r.status();
        if s.is_success() {
            return Ok(Ok(r.json().await?));
        }
        let body = r.text().await.unwrap_or_default();
        let why = format!("{s}: {}", body.chars().take(300).collect::<String>());
        if s.is_client_error() {
            Ok(Err(why))
        } else {
            anyhow::bail!("router answered {why} to a new session")
        }
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

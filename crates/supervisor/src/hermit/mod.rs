//! hermit's side (supervisor 15): what the supervisor consumes from hermit (the descriptor, secrets per key,
//! activity reports) and the `:7070` API it serves to hermit.
//!
//! - [`Hermit`]: the client at the fixed name `hermit` and the descriptor, read at start (retried 0.25 s doubling to
//!   2 s until it answers), at each `start_turn` and every 10 min with `If-None-Match`.
//! - [`activity`]: the activity loop (every `min(activityEverySeconds, 15)` s and at once on a state change).
//! - [`control`]: the `:7070` server (health, state, lifecycle, exec), every route behind the control token.
//! - [`exec`]: `POST /v1/exec`.

pub mod activity;
pub mod control;
pub mod exec;

use std::sync::Arc;

use hermit_api::client::ClientError;
use hermit_api::sandbox_surface::{SandboxSelf, SecretValue};
use hermit_api::server::SelfAnswer;
use tokio::sync::watch;

use crate::limits::{DESCRIPTOR_REFRESH, DESCRIPTOR_RETRY_FIRST, DESCRIPTOR_RETRY_MAX, HERMIT_TIMEOUT};

/// One call to hermit, given at most [`HERMIT_TIMEOUT`] (20).
async fn bounded<T>(f: impl std::future::Future<Output = Result<T, ClientError>>) -> Result<T, ClientError> {
    tokio::time::timeout(HERMIT_TIMEOUT, f)
        .await
        .unwrap_or(Err(ClientError::Timeout("hermit call")))
}

/// The fixed name.
pub const BASE: &str = "http://hermit";

/// The client and the current descriptor.
#[derive(Debug, Clone)]
pub struct Hermit {
    client: hermit_api::Client,
    descriptor: watch::Sender<Option<Arc<SandboxSelf>>>,
}

impl Hermit {
    /// Over the fixed-names client (no bearer: the proxy credential is the identity).
    #[must_use]
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_client(hermit_api::Client::new(http, BASE))
    }

    /// Over a prepared client (tests address a fake hermit directly).
    #[must_use]
    pub fn with_client(client: hermit_api::Client) -> Self {
        Self {
            client,
            descriptor: watch::channel(None).0,
        }
    }

    #[must_use]
    pub fn client(&self) -> &hermit_api::Client {
        &self.client
    }

    /// The descriptor, once read.
    #[must_use]
    pub fn descriptor(&self) -> Option<Arc<SandboxSelf>> {
        self.descriptor.borrow().clone()
    }

    #[must_use]
    pub fn descriptor_changes(&self) -> watch::Receiver<Option<Arc<SandboxSelf>>> {
        self.descriptor.subscribe()
    }

    /// Reads the descriptor until hermit answers (4.3 step 2): 0.25 s doubling to 2 s, forever.
    pub async fn fetch_initial(&self) -> Arc<SandboxSelf> {
        let mut delay = DESCRIPTOR_RETRY_FIRST;
        let mut logged = false;
        loop {
            match self.refresh().await {
                Ok(d) => return d,
                Err(e) => {
                    if !logged {
                        tracing::warn!(error = %e, "descriptor unavailable");
                        logged = true;
                    }
                    tokio::time::sleep(delay).await;
                    delay = (delay * 2).min(DESCRIPTOR_RETRY_MAX);
                }
            }
        }
    }

    /// One read with `If-None-Match`: the current descriptor (changed or not).
    ///
    /// # Errors
    /// hermit's or the transport's error; the last descriptor stays in place.
    pub async fn refresh(&self) -> Result<Arc<SandboxSelf>, ClientError> {
        let etag = self.descriptor().map(|d| d.etag.clone());
        match bounded(self.client.sandbox_self(etag.as_deref())).await? {
            SelfAnswer::Fresh(d) => {
                let d: Arc<SandboxSelf> = Arc::from(d);
                let old = self.descriptor.send_replace(Some(d.clone()));
                if old.as_ref().is_some_and(|o| o.etag != d.etag) {
                    tracing::info!(etag = %d.etag, "descriptor changed");
                }
                Ok(d)
            }
            SelfAnswer::NotModified { .. } => self.descriptor().ok_or_else(|| ClientError::Decode {
                status: 304,
                message: "304 without a descriptor".into(),
            }),
        }
    }

    /// The refresh task: every 10 min.
    pub async fn refresh_loop(self) {
        loop {
            tokio::time::sleep(DESCRIPTOR_REFRESH).await;
            if let Err(e) = self.refresh().await {
                tracing::info!(error = %e, "descriptor refresh failed");
            }
        }
    }

    /// A secret asked for by a process in the sandbox, from hermit at this moment (no cache, so a revocation takes
    /// effect at once). The key is logged, never the value.
    ///
    /// # Errors
    /// hermit's answer (404 `not_granted`) or the transport's.
    pub async fn secret(&self, key: &str) -> Result<SecretValue, ClientError> {
        tracing::info!(key = %key, "secret asked");
        bounded(self.client.sandbox_secret(key)).await
    }
}

/// `supervisor-<sha12>`.
#[must_use]
pub fn version() -> String {
    format!("supervisor-{}", aviary_core::version::sha12())
}

/// This process's resident bytes.
#[must_use]
pub fn own_rss() -> u64 {
    crate::memguard::Proc::system()
        .rss(std::process::id().cast_signed())
        .unwrap_or(0)
}

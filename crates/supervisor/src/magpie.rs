//! The supervisor's client of magpie (surface `magpie-sandbox`). WP-21 builds the client and the start's lease
//! step (supervisor 4.3 step 5); the `~/.claude` operations, commands and reports are WP-22's and WP-23's.

use std::time::Duration;

use magpie_api::errors::ErrorCode;
use magpie_api::sandbox::LeaseResponse;
use uuid::Uuid;

use crate::limits::{LEASE_RETRY_FIRST, LEASE_RETRY_MAX, LEASE_RETRY_NOT_CURRENT};

/// The fixed name.
pub const BASE: &str = "http://magpie";
/// What the client presents as its bearer: nothing, since the gateway strips it and adds its own pair token after
/// verifying the proxy credential (platform 3.11.3).
const NO_BEARER: &str = "sandbox";

/// The magpie client over the fixed-names client.
#[must_use]
pub fn client(http: reqwest::Client) -> magpie_api::client::Client {
    magpie_api::client::Client::new(http, BASE, NO_BEARER)
}

/// Why a lease attempt has to wait.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LeaseWait {
    /// `409 not_current_sandbox` or `conversation_migrating`: another sandbox is the live one, or the conversation
    /// is being moved. Retried every 30 s without exiting.
    NotCurrent,
    /// The lease names another conversation than hermit's descriptor (retried every 30 s).
    WrongConversation,
    /// 503, no answer, or another refusal: retried with backoff.
    Unavailable,
}

/// The delay before the next attempt.
#[must_use]
pub fn lease_delay(wait: LeaseWait, attempt: u32) -> Duration {
    match wait {
        LeaseWait::NotCurrent | LeaseWait::WrongConversation => LEASE_RETRY_NOT_CURRENT,
        LeaseWait::Unavailable => LEASE_RETRY_FIRST
            .saturating_mul(1 << attempt.min(16))
            .min(LEASE_RETRY_MAX),
    }
}

/// One attempt: the lease, or why to wait.
///
/// # Errors
/// Why the lease cannot be had now.
pub async fn try_lease(client: &magpie_api::client::Client, conversation: Uuid) -> Result<LeaseResponse, LeaseWait> {
    match client.sandbox(None).lease().await {
        Ok(l) if l.conversation_id == conversation => Ok(l),
        Ok(l) => {
            tracing::error!(lease = %l.conversation_id, descriptor = %conversation, "lease names another conversation");
            Err(LeaseWait::WrongConversation)
        }
        Err(e) => {
            if let Some(ErrorCode::NotCurrentSandbox | ErrorCode::ConversationMigrating) = e.code() {
                return Err(LeaseWait::NotCurrent);
            }
            tracing::info!(error = %e, "lease unavailable");
            Err(LeaseWait::Unavailable)
        }
    }
}

/// Takes the lease (4.3 step 5): retries until magpie gives it, never exits (so two live sandboxes of one
/// conversation never steal it from each other in a restart loop). `waiting` is called with each wait.
pub async fn take_lease(
    client: &magpie_api::client::Client,
    conversation: Uuid,
    mut waiting: impl FnMut(LeaseWait),
) -> LeaseResponse {
    let mut attempt = 0u32;
    loop {
        match try_lease(client, conversation).await {
            Ok(l) => {
                tracing::info!(epoch = l.epoch, "lease taken");
                return l;
            }
            Err(w) => {
                waiting(w);
                let d = lease_delay(w, attempt);
                attempt = if w == LeaseWait::Unavailable { attempt + 1 } else { 0 };
                tokio::time::sleep(d).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays() {
        assert_eq!(lease_delay(LeaseWait::NotCurrent, 9), Duration::from_secs(30));
        assert_eq!(lease_delay(LeaseWait::Unavailable, 0), Duration::from_millis(250));
        assert_eq!(lease_delay(LeaseWait::Unavailable, 2), Duration::from_secs(1));
        assert_eq!(lease_delay(LeaseWait::Unavailable, 40), Duration::from_secs(10));
    }
}

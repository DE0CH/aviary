//! The supervisor's client of magpie (surface `magpie-sandbox`): the client, the start's lease step (supervisor 4.3
//! step 5) and the `opId`s of the `~/.claude` operations (9.3); the operations themselves are in `fuse::remote`, the
//! commands and reports WP-23's.

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

/// The namespace of every cc operation's `opId` (9.3: `UUIDv5` of `bootId ‖ unique ‖ step`). Fixed for ever: a
/// replayed request after a restart must recompute the ids it used.
pub const SUPERVISOR_OP_NS: Uuid = Uuid::from_u128(0x5b1e_4c1d_9f3a_4e0b_8a6d_2c7f_0e91_d4a3);

/// The `opId` of the `step`-th magpie call of the FUSE request `unique` on the connection `boot_id`.
#[must_use]
pub fn op_id(boot_id: Uuid, unique: u64, step: u32) -> Uuid {
    let mut name = [0u8; 28];
    name[..16].copy_from_slice(boot_id.as_bytes());
    name[16..24].copy_from_slice(&unique.to_le_bytes());
    name[24..].copy_from_slice(&step.to_le_bytes());
    Uuid::new_v5(&SUPERVISOR_OP_NS, &name)
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
    fn op_ids_are_derived() {
        let b = Uuid::from_u128(7);
        assert_eq!(op_id(b, 9, 0), op_id(b, 9, 0));
        assert_ne!(op_id(b, 9, 0), op_id(b, 9, 1));
        assert_ne!(op_id(b, 9, 0), op_id(b, 10, 0));
        assert_ne!(op_id(b, 9, 0), op_id(Uuid::from_u128(8), 9, 0));
        assert_eq!(op_id(b, 9, 0).get_version_num(), 5);
    }

    #[test]
    fn delays() {
        assert_eq!(lease_delay(LeaseWait::NotCurrent, 9), Duration::from_secs(30));
        assert_eq!(lease_delay(LeaseWait::Unavailable, 0), Duration::from_millis(250));
        assert_eq!(lease_delay(LeaseWait::Unavailable, 2), Duration::from_secs(1));
        assert_eq!(lease_delay(LeaseWait::Unavailable, 40), Duration::from_secs(10));
    }
}

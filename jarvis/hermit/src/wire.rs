//! What a session machine's gateway (`aviary-gateway`) and hermit-jarvis's gateway listener say to each other. Every
//! path is under the public host `chat-gw.deyaochen.com` (Cloudflare Access, service token `aviary-gateway`), which
//! the services tunnel sends to hermit-jarvis's gateway port.
//!
//! - `POST /jarvis/boot`: the machine's signed boot (its Jarvis 4 machine key, checked by the router): which session
//!   it is, what the descriptor should say about this boot. Answers the sandbox token and control token.
//! - `POST /jarvis/health`: the supervisor's `:7070` health as the machine read it, while it starts and then now and
//!   again; hermit marks the sandbox ready from it.
//! - `POST /jarvis/deliver`: a prompt Jarvis delivers into the session's conversation (the first prompt, wakeups,
//!   watches, peer messages): a turn as the session's person.
//! - `ANY /magpie/<path>`, `ANY /hermit/<path>`: the sandbox's fixed names, forwarded as the contract's gateway does.
//!
//! All but the boot carry the sandbox token in [`TOKEN_HEADER`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// The sandbox token between the two halves (the machine's proxy credential, moved into a header Cloudflare passes).
pub const TOKEN_HEADER: &str = "x-aviary-sandbox-token";

pub const BOOT: &str = "/jarvis/boot";
pub const HEALTH: &str = "/jarvis/health";
pub const DELIVER: &str = "/jarvis/deliver";

/// The environment names a machine may hand to its sandbox's descriptor: Jarvis 4's own non-secret values, which
/// Jarvis's scripts read. Anything else is refused (no secret ever travels to hermit).
pub const BOOT_ENV_NAMES: &[&str] = &[
    "SESSION_ID",
    "SESSION_HARNESS",
    "SESSION_LABEL",
    "SESSION_MODEL",
    "SESSION_PERMISSION_MODE",
    "FLY_MACHINE_ID",
    "FLY_REGION",
    "JARVIS_URL",
    "LOBSTER_CHANNEL",
    "SESSION_API_TOKEN",
    "TZ",
];

/// `POST /jarvis/boot` body (signed with the machine key over `"POST /jarvis/boot <time> <sha256 hex of body>"`, in
/// the router's `X-Machine`, `X-Time`, `X-Sig` headers).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootRequest {
    /// The Jarvis 4 session id (`s…`); the router's answer must name the same.
    pub session: String,
    /// `sha256:<64 hex>`: the session image's digest.
    pub image: String,
    /// The machine's memory (the supervisor's memory guard limit).
    pub memory_bytes: u64,
    pub cpus: u32,
    /// Values from [`BOOT_ENV_NAMES`] only.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// `POST /jarvis/boot` answer.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootReply {
    pub sandbox_id: Uuid,
    pub conversation_id: Uuid,
    /// `hst1.…`: the proxy credential the supervisor and Claude Code present.
    pub token: String,
    /// `hct_…`: what the machine presents to the supervisor's `:7070` (its health).
    pub control_token: String,
}

impl std::fmt::Debug for BootReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootReply")
            .field("sandbox_id", &self.sandbox_id)
            .field("conversation_id", &self.conversation_id)
            .finish_non_exhaustive()
    }
}

/// `POST /jarvis/health` body: the supervisor's `GET /v1/health` answer as the machine read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub ok: bool,
    pub ready: bool,
    pub protocol: u32,
    pub version: String,
    pub boot_id: String,
    pub busy: bool,
    /// The supervisor's run state (`starting`, `running`, `stopping`, `held`, `asking`, `idle`, …); empty from a
    /// machine whose gateway predates it.
    #[serde(default)]
    pub state: String,
}

/// `POST /jarvis/health` answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReply {
    /// The sandbox's state (`starting`, `ready`, …).
    pub state: String,
    /// The conversation's title in the chat (magpie's: generated from the first message, or Deyao's rename), for the
    /// session's card in the Jarvis 4 app; none while it is untitled or magpie did not answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// `POST /jarvis/deliver` body.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliverRequest {
    /// The delivery's id: a repeat is the same turn (magpie's `clientMessageId`).
    pub id: String,
    pub text: String,
}

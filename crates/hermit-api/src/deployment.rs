//! H1 `GET /v1/deployment` (hermit 3.3): served from memory, under 4 KiB. The facts of the deployment: the product,
//! sign-in, and who starts conversations.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub product: Product,
    pub sign_in: SignIn,
    /// Absent = the default (people start conversations).
    #[serde(default, skip_serializing_if = "Conversations::is_default")]
    pub conversations: Conversations,
}

/// Who starts conversations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conversations {
    /// People start conversations in the app. False: the deployment starts every conversation itself (a session
    /// started elsewhere is one conversation, made through magpie's hermit route), so pigeon refuses a person's new
    /// conversation and tells the app (`GET /api/jarvis/me` `newConversations`), which offers none.
    #[serde(default = "yes")]
    pub people_create: bool,
    /// People pick each conversation's model from the models route's list (pigeon takes the app's pick and tells the
    /// app, `GET /api/jarvis/me` `modelPick`). False: every conversation runs the list's default, and the app shows
    /// its name read-only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub people_pick_model: bool,
    /// People delete and restore their conversations in the app. False: the deployment ends every conversation
    /// itself (a session's lifecycle is managed elsewhere; magpie's hermit routes close and delete them), so the app
    /// offers no delete and no Recently deleted (`GET /api/jarvis/me` `conversationDelete`).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub people_delete: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_true(b: &bool) -> bool {
    *b
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if signature
fn is_false(b: &bool) -> bool {
    !*b
}

fn yes() -> bool {
    true
}

impl Default for Conversations {
    fn default() -> Self {
        Self {
            people_create: true,
            people_pick_model: false,
            people_delete: true,
        }
    }
}

impl Conversations {
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Product {
    pub name: String,
    /// BCP 47, e.g. `zh-HK`.
    pub locale: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignIn {
    /// The Cloudflare Access applications pigeon accepts tokens from.
    pub access: Vec<AccessApp>,
    /// The iOS app's URL scheme; `null` = iOS sign-in off.
    pub app_scheme: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessApp {
    pub team_domain: String,
    pub audience: String,
}

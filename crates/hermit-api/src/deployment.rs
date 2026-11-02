//! H1 `GET /v1/deployment` (hermit 3.3): served from memory, under 4 KiB, no capability flags.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Deployment {
    pub product: Product,
    pub sign_in: SignIn,
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

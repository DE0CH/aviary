//! The operator page (hermit 3.11). The contract only says it exists, is read-only and holds no message contents;
//! the snapshot's content is the implementation's (a deployment's monitor fields are guarded by its hermit's own tests).

use serde::{Deserialize, Serialize};

/// `GET|HEAD /ops/snapshot?tz=<IANA zone>`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotQuery {
    #[serde(default)]
    pub tz: Option<String>,
}

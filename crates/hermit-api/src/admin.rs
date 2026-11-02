//! The admin routes (hermit 3.12, 16): the operator's, never a service's. Request bodies are typed; the listing
//! answers are the implementation's own JSON (the contract names their content, not their shape) and are passed as
//! `serde_json::Value`.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::time::Timestamp;

/// `PATCH /v1/admin/hosts/{name}`. `maxSandboxes`: absent = unchanged, `null` = by memory/CPU, 0 = no new
/// sandboxes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostPatch {
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_double",
        deserialize_with = "de_double"
    )]
    pub max_sandboxes: Option<Option<u32>>,
    /// Retire the host (`retire_by = operator`): its sandboxes move off, then it is terminated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retire: Option<bool>,
    /// Set a retiring host active again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactivate: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[allow(clippy::ref_option, clippy::option_option, clippy::trivially_copy_pass_by_ref)] // serde's serialize_with signature
fn ser_double<S: Serializer>(v: &Option<Option<u32>>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(inner) => inner.serialize(s),
        None => s.serialize_none(),
    }
}

#[allow(clippy::option_option)]
fn de_double<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<u32>>, D::Error> {
    Option::<u32>::deserialize(d).map(Some)
}

/// `PUT /v1/admin/sandboxes-enabled`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxesEnabled {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `POST /v1/admin/fresh-host` (`ttlMinutes` 1..=60).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FreshHost {
    pub ttl_minutes: u32,
}

/// `GET /v1/admin/sandboxes?conversation=&person=&since=`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminSandboxesQuery {
    #[serde(default)]
    pub conversation: Option<Uuid>,
    #[serde(default)]
    pub person: Option<String>,
    #[serde(default)]
    pub since: Option<Timestamp>,
}

/// `POST /v1/admin/sandboxes/{id}/release`: a forced `operator` release with this note.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminRelease {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// `POST /v1/admin/recycle`: release every live sandbox on another image (soft with `onlyIdle`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recycle {
    pub except_image: String,
    pub only_idle: bool,
}

/// `POST /v1/admin/people/{email}/purge` (the only deletion of a person's data; 409 `still_allowed`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeRequest {
    /// The same email as the path.
    pub confirm: String,
}

/// magpie's 202 answer, passed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PurgeReply {
    pub conversations: u64,
}

/// `GET /v1/admin/events?limit=`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminEventsQuery {
    #[serde(default)]
    pub limit: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::HostPatch;

    #[test]
    fn max_sandboxes_three_ways() {
        let absent: HostPatch = serde_json::from_str("{}").unwrap();
        assert_eq!(absent.max_sandboxes, None);
        let null: HostPatch = serde_json::from_str(r#"{"maxSandboxes": null}"#).unwrap();
        assert_eq!(null.max_sandboxes, Some(None));
        let zero: HostPatch = serde_json::from_str(r#"{"maxSandboxes": 0}"#).unwrap();
        assert_eq!(zero.max_sandboxes, Some(Some(0)));
        assert_eq!(serde_json::to_string(&null).unwrap(), r#"{"maxSandboxes":null}"#);
        assert_eq!(serde_json::to_string(&absent).unwrap(), "{}");
    }
}

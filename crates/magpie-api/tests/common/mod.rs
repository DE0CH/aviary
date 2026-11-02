//! Fixture loading and the typed checks shared by the fixture tests and the client/server test.
#![allow(
    dead_code,
    clippy::missing_panics_doc,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::needless_pass_by_value,
    clippy::unused_self
)]

use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Mutex;

use aviary_core::http::ApiError;
use magpie_api::errors::{ErrorCode, Refusal};
use magpie_api::routes;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

pub const TOKEN: &str = "test-pair-token-0000000000000000000000000000";
pub const PERSON: &str = "chan.tai.man@example.com";
pub const ADMIN: &str = "admin@example.com";
pub const LEASE: Uuid = Uuid::from_u128(0x7d2f_4e6a_8b9c_4d0e_a1f2_3b4c_5d6e_7f80);

pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Every `fixtures/<dir>/*.json`, by file stem, sorted.
pub fn fixture_files(dir: &str) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(fixtures_dir().join(dir)).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "json") {
            let name = path.file_stem().unwrap().to_string_lossy().into_owned();
            let v: Value = serde_json::from_slice(&std::fs::read(&path).unwrap())
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            out.push((name, v));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn route_fixture(id: &str) -> Value {
    let path = fixtures_dir().join("routes").join(format!("{id}.json"));
    let bytes = std::fs::read(&path).unwrap_or_else(|_| panic!("missing fixture {}", path.display()));
    serde_json::from_slice(&bytes).unwrap()
}

/// Parse `v` as `T` and serialise it back: the JSON must be unchanged.
pub fn round_trip<T: DeserializeOwned + Serialize>(v: &Value, what: &str) -> T {
    let t: T = serde_json::from_value(v.clone()).unwrap_or_else(|e| panic!("{what}: does not parse: {e}\n{v:#}"));
    let back = serde_json::to_value(&t).unwrap();
    assert_eq!(&back, v, "{what}: does not round-trip");
    t
}

pub fn check_params(id: &str, f: &Value) {
    let route = routes::by_id(id).unwrap();
    let want: BTreeSet<&str> = routes::params(route.path).into_iter().collect();
    let empty = serde_json::Map::new();
    let got = f.get("params").and_then(Value::as_object).unwrap_or(&empty);
    let got_keys: BTreeSet<&str> = got.keys().map(String::as_str).collect();
    assert_eq!(got_keys, want, "{id}: path params");
    for (k, v) in got {
        let s = v.as_str().unwrap_or_else(|| panic!("{id}: param {k} must be a string"));
        match k.as_str() {
            "id" | "turnId" => {
                Uuid::parse_str(s).unwrap_or_else(|_| panic!("{id}: {k} is not a uuid"));
            }
            "cmdSeq" => {
                s.parse::<u64>().unwrap_or_else(|_| panic!("{id}: {k} is not a number"));
            }
            _ => assert!(!s.is_empty(), "{id}: {k} is empty"),
        }
    }
}

/// One part of a route fixture (query, request, success response) and how to check it.
pub trait Part {
    fn check_query(id: &str, v: Option<&Value>);
    fn check_request(id: &str, f: &Value);
    fn check_success(id: &str, entry: &Value);
}

pub struct NoQuery;
pub struct NoBody;
pub struct RawBody;
pub struct NoContent;
pub struct RawBytes;
pub struct Json<T>(PhantomData<T>);
pub struct OptionJson<T>(PhantomData<T>);

impl Part for NoQuery {
    fn check_query(id: &str, v: Option<&Value>) {
        assert!(v.is_none(), "{id}: this route takes no query");
    }
    fn check_request(_: &str, _: &Value) {}
    fn check_success(_: &str, _: &Value) {}
}

impl Part for NoBody {
    fn check_query(_: &str, _: Option<&Value>) {}
    fn check_request(id: &str, f: &Value) {
        assert!(
            f.get("request").is_none() && f.get("requestBytes").is_none(),
            "{id}: this route takes no body"
        );
    }
    fn check_success(_: &str, _: &Value) {}
}

impl Part for RawBody {
    fn check_query(_: &str, _: Option<&Value>) {}
    fn check_request(id: &str, f: &Value) {
        assert!(
            f.get("requestBytes").and_then(Value::as_str).is_some(),
            "{id}: requestBytes"
        );
        assert!(f.get("request").is_none(), "{id}: a raw body route has no JSON request");
    }
    fn check_success(_: &str, _: &Value) {}
}

impl Part for NoContent {
    fn check_query(_: &str, _: Option<&Value>) {}
    fn check_request(_: &str, _: &Value) {}
    fn check_success(id: &str, entry: &Value) {
        assert_eq!(entry["status"], 204, "{id}");
        assert!(
            entry.get("body").is_none() && entry.get("bytes").is_none(),
            "{id}: 204 has no body"
        );
    }
}

impl Part for RawBytes {
    fn check_query(_: &str, _: Option<&Value>) {}
    fn check_request(_: &str, _: &Value) {}
    fn check_success(id: &str, entry: &Value) {
        assert!(
            entry.get("bytes").and_then(Value::as_str).is_some(),
            "{id}: raw success needs bytes"
        );
    }
}

impl<T: DeserializeOwned + Serialize> Part for Json<T> {
    fn check_query(id: &str, v: Option<&Value>) {
        let v = v.unwrap_or_else(|| panic!("{id}: query missing"));
        let _: T = round_trip(v, &format!("{id} query"));
    }
    fn check_request(id: &str, f: &Value) {
        let v = f.get("request").unwrap_or_else(|| panic!("{id}: request missing"));
        let _: T = round_trip(v, &format!("{id} request"));
    }
    fn check_success(id: &str, entry: &Value) {
        let v = entry
            .get("body")
            .unwrap_or_else(|| panic!("{id}: success body missing"));
        let _: T = round_trip(v, &format!("{id} {} body", entry["status"]));
    }
}

impl<T: DeserializeOwned + Serialize> Part for OptionJson<T> {
    fn check_query(_: &str, _: Option<&Value>) {}
    fn check_request(_: &str, _: &Value) {}
    fn check_success(id: &str, entry: &Value) {
        if entry["status"] == 204 {
            NoContent::check_success(id, entry);
        } else {
            Json::<T>::check_success(id, entry);
        }
    }
}

/// Check a route fixture: its query, its request, every success response, every error response.
pub fn check<Q: Part, B: Part, R: Part>(id: &str, f: &Value) {
    let route = routes::by_id(id).unwrap();
    Q::check_query(id, f.get("query"));
    B::check_request(id, f);
    let responses = f["responses"].as_array().unwrap_or_else(|| panic!("{id}: responses"));
    let mut errors = 0;
    let mut successes = 0;
    for entry in responses {
        let status = u16::try_from(entry["status"].as_u64().unwrap()).unwrap();
        if route.ok.contains(&status) {
            successes += 1;
            R::check_success(id, entry);
        } else {
            errors += 1;
            assert!(
                status >= 400,
                "{id}: status {status} is neither a success of the route nor an error"
            );
            let e: Refusal = round_trip(&entry["body"], &format!("{id} {status}"));
            assert!(
                !matches!(e.code, ErrorCode::Other(_)),
                "{id}: unknown error code {}",
                e.code
            );
            assert_eq!(
                e.code.status(),
                status,
                "{id}: {} is answered with {}",
                e.code,
                e.code.status()
            );
        }
    }
    assert!(successes >= 1, "{id}: no success response");
    assert!(errors >= 1, "{id}: no error response");
}

pub fn param_str(f: &Value, k: &str) -> String {
    f["params"][k].as_str().unwrap().to_owned()
}

pub fn param_uuid(f: &Value, k: &str) -> Uuid {
    Uuid::parse_str(&param_str(f, k)).unwrap()
}

pub fn param_u64(f: &Value, k: &str) -> u64 {
    param_str(f, k).parse().unwrap()
}

pub fn query<T: DeserializeOwned>(f: &Value) -> T {
    serde_json::from_value(f["query"].clone()).unwrap()
}

pub fn request<T: DeserializeOwned>(f: &Value) -> T {
    serde_json::from_value(f["request"].clone()).unwrap()
}

pub fn header(f: &Value, k: &str) -> String {
    f["headers"][k].as_str().unwrap().to_owned()
}

pub fn request_bytes(f: &Value) -> Vec<u8> {
    f["requestBytes"].as_str().unwrap().as_bytes().to_vec()
}

fn status_of(entry: &Value) -> u16 {
    u16::try_from(entry["status"].as_u64().unwrap()).unwrap()
}

pub fn first_success(id: &str) -> Value {
    let route = routes::by_id(id).unwrap();
    let f = route_fixture(id);
    f["responses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| route.ok.contains(&status_of(e)))
        .unwrap()
        .clone()
}

pub fn first_error(id: &str) -> (u16, Value) {
    let route = routes::by_id(id).unwrap();
    let f = route_fixture(id);
    let e = f["responses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| !route.ok.contains(&status_of(e)))
        .unwrap()
        .clone();
    (status_of(&e), e["body"].clone())
}

/// What the client returns for the first success response, as JSON (see `client_server.rs`'s `call`).
pub fn expected_success(id: &str) -> Value {
    let route = routes::by_id(id).unwrap();
    let e = first_success(id);
    let status = status_of(&e);
    if let Some(bytes) = e.get("bytes") {
        return json!({"status": status, "bytes": bytes});
    }
    let Some(body) = e.get("body") else { return Value::Null };
    let admitted = route.ok.contains(&200) && route.ok.iter().any(|s| matches!(s, 201 | 202));
    let empty = body == &json!({});
    match (admitted, empty) {
        (true, true) => json!({"new": status != 200}),
        (true, false) => json!({"new": status != 200, "body": body}),
        (false, true) => Value::Null,
        (false, false) => body.clone(),
    }
}

pub async fn raw_value(
    r: Result<reqwest::Response, magpie_api::client::ClientError>,
) -> Result<Value, magpie_api::client::ClientError> {
    let resp = r?;
    let status = resp.status().as_u16();
    let bytes = resp.bytes().await.unwrap();
    Ok(json!({"status": status, "bytes": String::from_utf8(bytes.to_vec()).unwrap()}))
}

/// A server that checks each request against its route fixture and answers from it.
pub struct FixtureServer {
    fail: bool,
    seen: Mutex<BTreeSet<String>>,
}

impl FixtureServer {
    pub fn new(fail: bool) -> Self {
        FixtureServer {
            fail,
            seen: Mutex::new(BTreeSet::new()),
        }
    }

    pub fn received(
        &self,
        id: &str,
        ctx: &magpie_api::server::Ctx,
        params: &Value,
        query: Option<Value>,
        body: Option<Value>,
    ) {
        let route = routes::by_id(id).unwrap();
        let f = route_fixture(id);
        assert_eq!(
            &f.get("params").cloned().unwrap_or_else(|| json!({})),
            params,
            "{id}: path params"
        );
        assert_eq!(f.get("query").cloned(), query, "{id}: query");
        assert_eq!(f.get("request").cloned(), body, "{id}: body");
        assert_eq!(ctx.bearer.as_deref(), Some(TOKEN), "{id}: bearer");
        assert!(
            ctx.request_id.as_deref().is_some_and(|r| Uuid::parse_str(r).is_ok()),
            "{id}: Aviary-Request"
        );
        let protocol = match route.surface {
            routes::Surface::Person => format!("magpie-person={}", magpie_api::PROTOCOL_PERSON),
            routes::Surface::Sandbox => format!("magpie-sandbox={}", magpie_api::PROTOCOL_SANDBOX),
        };
        assert_eq!(
            ctx.protocol.as_deref(),
            Some(protocol.as_str()),
            "{id}: Aviary-Protocol"
        );
        if route.person {
            assert_eq!(ctx.person.as_deref(), Some(PERSON), "{id}: Aviary-Person");
        } else {
            assert_eq!(ctx.person, None, "{id}: no Aviary-Person");
        }
        if route.lease {
            assert_eq!(
                ctx.lease.as_deref(),
                Some(LEASE.to_string().as_str()),
                "{id}: Aviary-Lease"
            );
        }
        if route.admin {
            assert_eq!(ctx.admin.as_deref(), Some(ADMIN), "{id}: Aviary-Admin");
        }
        self.seen.lock().unwrap().insert(id.to_owned());
    }

    pub fn check_upload(&self, meta: &magpie_api::uploads::UploadMeta) {
        let f = route_fixture("upload");
        assert_eq!(meta.name, header(&f, "aviary-upload-name"));
        assert_eq!(meta.kind.as_str(), header(&f, "aviary-upload-kind"));
        assert_eq!(meta.content_type, header(&f, "content-type"));
    }

    pub async fn check_bytes(&self, id: &str, body: axum::body::Body) {
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        assert_eq!(bytes.to_vec(), request_bytes(&route_fixture(id)), "{id}: request bytes");
    }

    fn error(&self, id: &str) -> Option<ApiError> {
        if !self.fail {
            return None;
        }
        let (status, body) = first_error(id);
        Some(
            serde_json::from_value::<Refusal>(body)
                .unwrap()
                .with_status(status)
                .into(),
        )
    }

    pub fn answer_json<T: DeserializeOwned>(&self, id: &str) -> Result<T, ApiError> {
        if let Some(e) = self.error(id) {
            return Err(e);
        }
        Ok(serde_json::from_value(first_success(id)["body"].clone()).unwrap())
    }

    pub fn answer_admitted<T: DeserializeOwned>(&self, id: &str) -> Result<magpie_api::Admitted<T>, ApiError> {
        if let Some(e) = self.error(id) {
            return Err(e);
        }
        let entry = first_success(id);
        let body = serde_json::from_value(entry["body"].clone()).unwrap();
        Ok(if entry["status"] == 200 {
            magpie_api::Admitted::Repeat(body)
        } else {
            magpie_api::Admitted::New(body)
        })
    }

    pub fn answer_empty(&self, id: &str) -> Result<(), ApiError> {
        self.error(id).map_or(Ok(()), Err)
    }

    pub fn answer_unit(&self, id: &str) -> Result<(), ApiError> {
        self.error(id).map_or(Ok(()), Err)
    }

    pub fn answer_option<T: DeserializeOwned>(&self, id: &str) -> Result<Option<T>, ApiError> {
        if let Some(e) = self.error(id) {
            return Err(e);
        }
        let entry = first_success(id);
        if entry["status"] == 204 {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(entry["body"].clone()).unwrap()))
    }

    pub fn answer_raw(&self, id: &str) -> Result<axum::response::Response, ApiError> {
        if let Some(e) = self.error(id) {
            return Err(e);
        }
        let entry = first_success(id);
        let bytes = entry["bytes"].as_str().unwrap().as_bytes().to_vec();
        let mut resp = axum::response::Response::new(axum::body::Body::from(bytes));
        *resp.status_mut() = axum::http::StatusCode::from_u16(status_of(&entry)).unwrap();
        Ok(resp)
    }

    pub fn assert_saw_every_route(&self) {
        let seen = self.seen.lock().unwrap();
        for r in routes::ROUTES {
            assert!(seen.contains(r.id), "the server never received {}", r.id);
        }
    }
}

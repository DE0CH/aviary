//! The client and the router agree: a stub `Server` answering with the golden fixtures is served by `router` and
//! every client method gets the fixture back, with the right status, headers and inputs.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::unused_async_trait_impl,
    clippy::needless_pass_by_value
)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use hermit_api::admin::{
    AdminEventsQuery, AdminRelease, AdminSandboxesQuery, FreshHost, HostPatch, PurgeReply, PurgeRequest, Recycle,
    SandboxesEnabled,
};
use hermit_api::deployment::Deployment;
use hermit_api::exec::{ExecRequest, ExecResult};
use hermit_api::ops::SnapshotQuery;
use hermit_api::people::{People, Person, WarmReply, WarmRequest};
use hermit_api::sandbox::{
    Cursor, PutSandbox, PutSandboxRequest, ReleaseSandboxReply, ReleaseSandboxRequest, Sandbox, SandboxEvents,
    SandboxEventsQuery, SandboxQuery, SnapshotReply,
};
use hermit_api::sandbox_surface::{ActivityReply, ActivityReport, SandboxSelf, SecretKeys, SecretValue};
use hermit_api::server::{Answer, Ctx, SandboxIdentity, SelfAnswer, Server};
use hermit_api::token::{TokenKeys, TokenLiveness};
use hermit_api::workspaces::{
    ByteRange, CloneRequest, Entries, EntriesQuery, FileContent, FileMeta, FilesQuery, PutWorkspace, Workspace,
};
use hermit_api::{ApiError, Client, Timestamp, routes};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

fn fx<T: DeserializeOwned>(name: &str) -> T {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join(format!("{name}.json"));
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

const TOKEN: &str = "pair-token-for-tests-0000000000000000000000";

/// Records what each call saw.
#[derive(Default)]
struct Stub {
    seen: Mutex<Vec<String>>,
}

impl Stub {
    fn saw(&self, ctx: &Ctx, what: String) {
        assert_eq!(ctx.bearer(), Some(TOKEN), "{}", ctx.route);
        let (surface, n) = ctx.protocol().unwrap();
        let want = if ctx.route.starts_with(routes::SANDBOX_SURFACE_PREFIX) {
            "hermit-sandbox"
        } else {
            "hermit-core"
        };
        assert_eq!((surface, n), (want, 1), "{}", ctx.route);
        assert_eq!(ctx.request_id(), Some("0190f6c2-0000-7000-8000-00000000000a"));
        self.seen
            .lock()
            .unwrap()
            .push(format!("{} {} {what}", ctx.method, ctx.route));
    }
}

fn cid() -> Uuid {
    "0190f6c2-4a0d-7b2c-8e1f-0a1b2c3d4e5f".parse().unwrap()
}

impl Server for Stub {
    async fn deployment(&self, ctx: Ctx) -> Answer<Deployment> {
        self.saw(&ctx, String::new());
        Ok(fx("deployment"))
    }
    async fn person(&self, ctx: Ctx, email: String) -> Answer<Person> {
        self.saw(&ctx, email);
        Ok(fx("person-staff"))
    }
    async fn people(&self, ctx: Ctx) -> Answer<People> {
        self.saw(&ctx, String::new());
        Ok(fx("people"))
    }
    async fn warm(&self, ctx: Ctx, body: WarmRequest) -> Answer<WarmReply> {
        assert_eq!(body, fx("warm-request"));
        self.saw(&ctx, String::new());
        Ok(fx("warm-reply"))
    }
    async fn put_sandbox(&self, ctx: Ctx, c: Uuid, body: PutSandboxRequest) -> Answer<PutSandbox> {
        assert_eq!(c, cid());
        assert_eq!(body, fx("put-sandbox-request"));
        self.saw(&ctx, String::new());
        Ok(PutSandbox::Starting(fx("sandbox-starting")))
    }
    async fn get_sandbox(&self, ctx: Ctx, _: Uuid, q: SandboxQuery) -> Answer<Sandbox> {
        self.saw(&ctx, format!("{q:?}"));
        Ok(fx("sandbox-ready"))
    }
    async fn release_sandbox(&self, ctx: Ctx, _: Uuid, body: ReleaseSandboxRequest) -> Answer<ReleaseSandboxReply> {
        assert_eq!(body, fx("release-sandbox-request"));
        self.saw(&ctx, String::new());
        Ok(fx("release-sandbox-reply"))
    }
    async fn snapshot_sandbox(&self, ctx: Ctx, _: Uuid) -> Answer<SnapshotReply> {
        self.saw(&ctx, String::new());
        Ok(fx("snapshot-reply"))
    }
    async fn sandbox_events(&self, ctx: Ctx, q: SandboxEventsQuery) -> Answer<SandboxEvents> {
        self.saw(&ctx, format!("{q:?}"));
        Ok(fx("sandbox-events"))
    }
    async fn put_workspace(&self, ctx: Ctx, _: Uuid) -> Answer<PutWorkspace> {
        self.saw(&ctx, String::new());
        Ok(PutWorkspace::Created(fx("workspace")))
    }
    async fn get_workspace(&self, ctx: Ctx, _: Uuid) -> Answer<Workspace> {
        self.saw(&ctx, String::new());
        Ok(fx("workspace"))
    }
    async fn delete_workspace(&self, ctx: Ctx, _: Uuid) -> Answer<()> {
        self.saw(&ctx, String::new());
        Ok(())
    }
    async fn workspace_entries(&self, ctx: Ctx, _: Uuid, q: EntriesQuery) -> Answer<Entries> {
        self.saw(&ctx, format!("{q:?}"));
        Ok(fx("entries"))
    }
    async fn workspace_file(&self, ctx: Ctx, _: Uuid, q: FilesQuery, range: Option<ByteRange>) -> Answer<FileContent> {
        self.saw(&ctx, format!("{} {range:?}", q.path));
        let all = b"hello, workspace";
        let size = all.len() as u64;
        let meta = |served| FileMeta {
            size,
            mtime: Timestamp::parse("2026-11-02T04:00:00Z").unwrap(),
            etag: FileMeta::etag_for(size, 1_793_592_000_000_000_000),
            served,
        };
        Ok(match range {
            None => FileContent::from_bytes(meta(None), Bytes::from_static(all)),
            Some(r) => {
                let (first, last) = r
                    .resolve(size)
                    .ok_or_else(|| hermit_api::error::range_not_satisfiable(size))?;
                let slice =
                    Bytes::copy_from_slice(&all[usize::try_from(first).unwrap()..=usize::try_from(last).unwrap()]);
                FileContent::from_bytes(meta(Some((first, last))), slice)
            }
        })
    }
    async fn clone_workspace(&self, ctx: Ctx, _: Uuid, body: CloneRequest) -> Answer<Workspace> {
        assert_eq!(body, fx("clone-request"));
        self.saw(&ctx, String::new());
        Ok(fx("workspace"))
    }
    async fn token_keys(&self, ctx: Ctx) -> Answer<TokenKeys> {
        self.saw(&ctx, String::new());
        Ok(fx("token-keys"))
    }
    async fn token_liveness(&self, ctx: Ctx, id: Uuid) -> Answer<TokenLiveness> {
        self.saw(&ctx, id.to_string());
        Ok(fx("token-liveness"))
    }
    async fn sandbox_self(&self, ctx: Ctx, inm: Option<String>) -> Answer<SelfAnswer> {
        let me = ctx.sandbox().unwrap();
        assert_eq!(me.person, "staff@example.org");
        self.saw(&ctx, format!("{inm:?}"));
        let d: SandboxSelf = fx("sandbox-self");
        if inm.as_deref() == Some(d.etag.as_str()) {
            Ok(SelfAnswer::NotModified { etag: d.etag })
        } else {
            Ok(SelfAnswer::Fresh(Box::new(d)))
        }
    }
    async fn sandbox_secrets(&self, ctx: Ctx) -> Answer<SecretKeys> {
        self.saw(&ctx, String::new());
        Ok(fx("secret-keys"))
    }
    async fn sandbox_secret(&self, ctx: Ctx, key: String) -> Answer<SecretValue> {
        self.saw(&ctx, key.clone());
        if key == "EXAMPLE_TOKEN" {
            Ok(fx("secret-value"))
        } else {
            Err(hermit_api::error::not_found(
                hermit_api::error::codes::NOT_GRANTED,
                "key not granted",
            ))
        }
    }
    async fn sandbox_activity(&self, ctx: Ctx, body: ActivityReport) -> Answer<ActivityReply> {
        assert_eq!(body, fx("activity-report"));
        self.saw(&ctx, String::new());
        Ok(fx("activity-reply-releasing"))
    }
    async fn ops_page(&self, ctx: Ctx) -> Answer<String> {
        self.saw(&ctx, ctx.person().unwrap_or_default().to_owned());
        Ok("<!doctype html><title>ops</title>".into())
    }
    async fn ops_snapshot(&self, ctx: Ctx, q: SnapshotQuery) -> Answer<Value> {
        self.saw(&ctx, format!("{q:?}"));
        Ok(json!({"tz": q.tz}))
    }
    async fn admin_hosts(&self, ctx: Ctx) -> Answer<Value> {
        assert_eq!(ctx.admin(), Some("operator@example.org"));
        self.saw(&ctx, String::new());
        Ok(json!({"hosts": []}))
    }
    async fn admin_patch_host(&self, ctx: Ctx, name: String, body: HostPatch) -> Answer<Value> {
        assert_eq!(body, fx("admin-host-patch-uncap"));
        self.saw(&ctx, name);
        Ok(json!({}))
    }
    async fn admin_sandboxes_enabled(&self, ctx: Ctx, body: SandboxesEnabled) -> Answer<Value> {
        assert_eq!(body, fx("admin-sandboxes-enabled"));
        self.saw(&ctx, String::new());
        Ok(json!({}))
    }
    async fn admin_fresh_host(&self, ctx: Ctx, body: FreshHost) -> Answer<Value> {
        assert_eq!(body, fx("admin-fresh-host"));
        self.saw(&ctx, String::new());
        Ok(json!({}))
    }
    async fn admin_sandboxes(&self, ctx: Ctx, q: AdminSandboxesQuery) -> Answer<Value> {
        self.saw(&ctx, format!("{q:?}"));
        Ok(json!([]))
    }
    async fn admin_sandbox_log(&self, ctx: Ctx, _: Uuid) -> Answer<Value> {
        self.saw(&ctx, String::new());
        Ok(json!({"log": ""}))
    }
    async fn admin_release(&self, ctx: Ctx, _: Uuid, body: AdminRelease) -> Answer<Value> {
        assert_eq!(body, fx("admin-release"));
        self.saw(&ctx, String::new());
        Ok(json!({}))
    }
    async fn admin_exec(&self, ctx: Ctx, _: Uuid, body: ExecRequest) -> Answer<ExecResult> {
        assert_eq!(body, fx("exec-request"));
        self.saw(&ctx, String::new());
        Ok(fx("exec-result"))
    }
    async fn admin_recycle(&self, ctx: Ctx, body: Recycle) -> Answer<Value> {
        assert_eq!(body, fx("admin-recycle"));
        self.saw(&ctx, String::new());
        Ok(json!({}))
    }
    async fn admin_purge(&self, ctx: Ctx, email: String, body: PurgeRequest) -> Answer<PurgeReply> {
        assert_eq!(email, body.confirm);
        self.saw(&ctx, email);
        Ok(fx("admin-purge-reply"))
    }
    async fn admin_events(&self, ctx: Ctx, q: AdminEventsQuery) -> Answer<Value> {
        self.saw(&ctx, format!("{q:?}"));
        Ok(json!([]))
    }
}

async fn serve<S: Server>(s: Arc<S>) -> String {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, hermit_api::router(s)).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn every_route_through_client_and_router() {
    let stub = Arc::new(Stub::default());
    let base = serve(stub.clone()).await;
    let c = Client::new(reqwest::Client::new(), base)
        .bearer(TOKEN)
        .request_id("0190f6c2-0000-7000-8000-00000000000a");
    let sid: Uuid = "0190f6c2-5b1e-7a3c-9d4e-1f2a3b4c5d6e".parse().unwrap();

    assert_eq!(c.deployment().await.unwrap(), fx::<Deployment>("deployment"));
    assert_eq!(
        c.person("Staff+x@example.org").await.unwrap(),
        fx::<Person>("person-staff")
    );
    assert_eq!(c.people().await.unwrap(), fx::<People>("people"));
    assert_eq!(
        c.warm(&fx("warm-request")).await.unwrap(),
        fx::<WarmReply>("warm-reply")
    );
    let put = c.put_sandbox(cid(), &fx("put-sandbox-request")).await.unwrap();
    assert_eq!(put, PutSandbox::Starting(fx("sandbox-starting")));
    let q = SandboxQuery {
        wait_ms: Some(25_000),
        after_version: Some(3),
    };
    assert_eq!(c.get_sandbox(cid(), q).await.unwrap(), fx::<Sandbox>("sandbox-ready"));
    assert_eq!(
        c.release_sandbox(cid(), &fx("release-sandbox-request")).await.unwrap(),
        fx::<ReleaseSandboxReply>("release-sandbox-reply")
    );
    // not implemented by the stub: the trait's default
    let e = c.pause_sandbox(cid()).await.unwrap_err();
    assert_eq!((e.status(), e.code()), (Some(501), Some("not_supported")));
    assert_eq!(c.resume_sandbox(cid()).await.unwrap_err().status(), Some(501));
    assert_eq!(
        c.snapshot_sandbox(cid()).await.unwrap(),
        fx::<SnapshotReply>("snapshot-reply")
    );
    let q = SandboxEventsQuery {
        after: Cursor::Latest,
        limit: Some(500),
        wait_ms: Some(0),
    };
    assert_eq!(
        c.sandbox_events(q).await.unwrap(),
        fx::<SandboxEvents>("sandbox-events")
    );
    let q = SandboxEventsQuery {
        after: Cursor::Seq(98121),
        limit: None,
        wait_ms: None,
    };
    c.sandbox_events(q).await.unwrap();

    assert_eq!(
        c.put_workspace(cid()).await.unwrap(),
        PutWorkspace::Created(fx("workspace"))
    );
    assert_eq!(c.get_workspace(cid()).await.unwrap(), fx::<Workspace>("workspace"));
    c.delete_workspace(cid()).await.unwrap();
    let q = EntriesQuery {
        path: Some("uploads/a b".into()),
        cursor: Some("img".into()),
    };
    assert_eq!(c.workspace_entries(cid(), &q).await.unwrap(), fx::<Entries>("entries"));
    let fq = FilesQuery {
        path: "uploads/a&b.txt".into(),
    };
    let whole = c.workspace_file(cid(), &fq, None).await.unwrap();
    assert_eq!((whole.meta.size, whole.meta.served), (16, None));
    assert_eq!(whole.meta.mtime, Timestamp::parse("2026-11-02T04:00:00Z").unwrap());
    assert_eq!(whole.meta.etag, "\"16-1793592000000000000\"");
    assert_eq!(&whole.bytes().await.unwrap()[..], b"hello, workspace");
    let part = c
        .workspace_file(cid(), &fq, Some(ByteRange { start: 7, end: None }))
        .await
        .unwrap();
    assert_eq!((part.meta.size, part.meta.served), (16, Some((7, 15))));
    assert_eq!(&part.bytes().await.unwrap()[..], b"workspace");
    let e = c
        .workspace_file(cid(), &fq, Some(ByteRange { start: 16, end: None }))
        .await
        .unwrap_err();
    assert_eq!((e.status(), e.code()), (Some(416), Some("bad_range")));
    assert_eq!(
        c.clone_workspace(cid(), &fx("clone-request")).await.unwrap(),
        fx::<Workspace>("workspace")
    );

    assert_eq!(c.token_keys().await.unwrap(), fx::<TokenKeys>("token-keys"));
    let live = c.token_liveness(sid, std::time::Duration::from_secs(1)).await.unwrap();
    assert_eq!(live, fx::<TokenLiveness>("token-liveness"));

    let sc = c.clone().sandbox_identity(&SandboxIdentity {
        id: sid,
        conversation: cid(),
        person: "staff@example.org".into(),
    });
    let d: SandboxSelf = fx("sandbox-self");
    assert_eq!(
        sc.sandbox_self(None).await.unwrap(),
        SelfAnswer::Fresh(Box::new(d.clone()))
    );
    assert_eq!(
        sc.sandbox_self(Some(&d.etag)).await.unwrap(),
        SelfAnswer::NotModified { etag: d.etag.clone() }
    );
    assert_eq!(sc.sandbox_secrets().await.unwrap(), fx::<SecretKeys>("secret-keys"));
    assert_eq!(
        sc.sandbox_secret("EXAMPLE_TOKEN").await.unwrap(),
        fx::<SecretValue>("secret-value")
    );
    let e = sc.sandbox_secret("OTHER").await.unwrap_err();
    assert_eq!((e.status(), e.code()), (Some(404), Some("not_granted")));
    assert_eq!(
        sc.sandbox_activity(&fx("activity-report")).await.unwrap(),
        fx::<ActivityReply>("activity-reply-releasing")
    );

    let pc = c.clone().as_person("admin@example.net");
    assert!(pc.ops_page().await.unwrap().starts_with("<!doctype html>"));
    assert_eq!(
        pc.ops_snapshot(Some("Asia/Hong_Kong")).await.unwrap(),
        json!({"tz": "Asia/Hong_Kong"})
    );

    let ac = c.clone().as_admin("operator@example.org");
    ac.admin_hosts().await.unwrap();
    ac.admin_patch_host("host-3f1c2a", &fx("admin-host-patch-uncap"))
        .await
        .unwrap();
    ac.admin_sandboxes_enabled(&fx("admin-sandboxes-enabled"))
        .await
        .unwrap();
    ac.admin_fresh_host(&fx("admin-fresh-host")).await.unwrap();
    let since = Timestamp::parse("2026-11-01T00:00:00Z").unwrap();
    let q = AdminSandboxesQuery {
        conversation: Some(cid()),
        person: Some("staff@example.org".into()),
        since: Some(since),
    };
    ac.admin_sandboxes(&q).await.unwrap();
    ac.admin_sandbox_log(sid).await.unwrap();
    ac.admin_release(sid, &fx("admin-release")).await.unwrap();
    assert_eq!(
        ac.admin_exec(sid, &fx("exec-request")).await.unwrap(),
        fx::<ExecResult>("exec-result")
    );
    ac.admin_recycle(&fx("admin-recycle")).await.unwrap();
    assert_eq!(
        ac.admin_purge("leaver@example.org", &fx("admin-purge-request"))
            .await
            .unwrap(),
        fx::<PurgeReply>("admin-purge-reply")
    );
    ac.admin_events(AdminEventsQuery { limit: Some(50) }).await.unwrap();

    let seen = stub.seen.lock().unwrap().clone();
    let has = |s: &str| assert!(seen.iter().any(|x| x == s), "missing {s:?} in {seen:#?}");
    has("GET /v1/people/{email} Staff+x@example.org");
    has("GET /v1/conversations/{cid}/sandbox SandboxQuery { wait_ms: Some(25000), after_version: Some(3) }");
    has("GET /v1/sandbox-events SandboxEventsQuery { after: Latest, limit: Some(500), wait_ms: Some(0) }");
    has("GET /v1/sandbox-events SandboxEventsQuery { after: Seq(98121), limit: None, wait_ms: None }");
    has("GET /v1/workspaces/{wid}/entries EntriesQuery { path: Some(\"uploads/a b\"), cursor: Some(\"img\") }");
    has("GET /v1/workspaces/{wid}/files uploads/a&b.txt Some(ByteRange { start: 7, end: None })");
    has("GET /ops/ admin@example.net");
    has("PATCH /v1/admin/hosts/{name} host-3f1c2a");
    has("POST /v1/admin/people/{email}/purge leaver@example.org");
    has(&format!("GET /v1/sandbox/self Some({:?})", d.etag));
}

/// A server that implements nothing.
struct Nothing;
impl Server for Nothing {}

#[tokio::test]
async fn transport_rules() {
    let base = serve(Arc::new(Nothing)).await;
    let http = reqwest::Client::new();

    // the default answer, with the common headers
    let res = http.get(format!("{base}{}", routes::DEPLOYMENT)).send().await.unwrap();
    assert_eq!(res.status(), 501);
    assert_eq!(res.headers()["cache-control"], "no-store");
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    assert_eq!(res.headers()["content-type"], "application/json; charset=utf-8");
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["code"], "not_supported");

    // unknown path, wrong method, bad uuid, bad JSON, too large: all in the envelope
    for (method, path, body, status, code) in [
        ("GET", "/v1/nothing".to_owned(), None, 404, "not_found"),
        ("GET", "/ops/other".to_owned(), None, 404, "not_found"),
        ("DELETE", routes::DEPLOYMENT.to_owned(), None, 405, "method_not_allowed"),
        ("POST", routes::OPS_PAGE.to_owned(), None, 405, "method_not_allowed"),
        (
            "GET",
            "/v1/conversations/not-a-uuid/sandbox".to_owned(),
            None,
            400,
            "bad_request",
        ),
        ("POST", routes::WARM.to_owned(), Some(b"{".to_vec()), 400, "bad_request"),
        (
            "POST",
            routes::WARM.to_owned(),
            Some(vec![b' '; 1024 * 1024 + 1]),
            413,
            "too_large",
        ),
        (
            "GET",
            "/v1/sandbox-events?after=soon".to_owned(),
            None,
            400,
            "bad_request",
        ),
    ] {
        let mut rb = http.request(method.parse().unwrap(), format!("{base}{path}"));
        if let Some(b) = body {
            rb = rb.header("content-type", "application/json").body(b);
        }
        let res = rb.send().await.unwrap();
        assert_eq!(res.status(), status, "{method} {path}");
        assert_eq!(res.headers()["cache-control"], "no-store", "{method} {path}");
        let v: Value = res.json().await.unwrap();
        assert_eq!(v["code"], code, "{method} {path}");
    }

    // unknown request fields are ignored (the 501 proves the body parsed)
    let res = http
        .post(format!("{base}{}", routes::WARM))
        .json(&json!({"person": "a@example.org", "conversationId": null, "later": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 501);
}

#[tokio::test]
async fn errors_carry_status_retry_after_and_extra_fields() {
    struct Busy;
    impl Server for Busy {
        async fn put_sandbox(&self, _: Ctx, _: Uuid, _: PutSandboxRequest) -> Answer<PutSandbox> {
            Err(hermit_api::error::sandboxes_disabled())
        }
        async fn deployment(&self, _: Ctx) -> Answer<Deployment> {
            Err(ApiError::unauthenticated())
        }
    }
    let base = serve(Arc::new(Busy)).await;
    let c = Client::new(reqwest::Client::new(), base.clone());
    let e = c.put_sandbox(cid(), &fx("put-sandbox-request")).await.unwrap_err();
    let hermit_api::ClientError::Api(api) = e else {
        panic!("{e}")
    };
    assert_eq!(api.status, 503);
    assert!(api.is("sandbox_busy"));
    assert_eq!(api.retry_after, Some(300));
    let want: hermit_api::ErrorBody = fx("error-sandboxes-disabled");
    assert_eq!(api.body, want);
    let res = reqwest::get(format!("{base}{}", routes::DEPLOYMENT)).await.unwrap();
    assert_eq!(res.status(), 401);
    assert_eq!(res.headers()["www-authenticate"], "Bearer");
}

#[test]
fn the_client_builds_paths_only_from_route_constants() {
    let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/client.rs")).unwrap();
    for (i, line) in src.lines().enumerate() {
        let code = line.split("//").next().unwrap_or_default();
        assert!(
            !code.contains("\"/v1") && !code.contains("\"/ops"),
            "client.rs:{}: a literal path",
            i + 1
        );
    }
}

#[tokio::test]
async fn unsupported_ranges_are_ignored_and_unsatisfiable_ones_are_416() {
    let base = serve(Arc::new(Stub::default())).await;
    let http = reqwest::Client::new();
    let url = format!(
        "{base}{}?path=a.txt",
        routes::fill(routes::WORKSPACE_FILES, &[&cid().to_string()])
    );
    let get = |range: &'static str| {
        http.get(&url)
            .bearer_auth(TOKEN)
            .header("aviary-protocol", "hermit-core=1")
            .header("aviary-request", "0190f6c2-0000-7000-8000-00000000000a")
            .header("range", range)
            .send()
    };
    // a suffix range, a list, another unit, last < first, bad syntax: the whole file
    for range in ["bytes=-5", "bytes=0-1,4-5", "items=0-1", "bytes=9-1", "bytes=x-"] {
        let res = get(range).await.unwrap();
        assert_eq!(res.status(), 200, "{range}");
        assert_eq!(res.headers()["content-length"], "16", "{range}");
        assert_eq!(&res.bytes().await.unwrap()[..], b"hello, workspace", "{range}");
    }
    let res = get("bytes=2-3").await.unwrap();
    assert_eq!(res.status(), 206);
    assert_eq!(res.headers()["content-range"], "bytes 2-3/16");
    assert_eq!(res.headers()["content-length"], "2");
    assert_eq!(&res.bytes().await.unwrap()[..], b"ll");
    let res = get("bytes=16-").await.unwrap();
    assert_eq!(res.status(), 416);
    assert_eq!(res.headers()["content-range"], "bytes */16");
    let v: Value = res.json().await.unwrap();
    assert_eq!(v["code"], "bad_range");
}

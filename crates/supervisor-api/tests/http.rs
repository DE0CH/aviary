//! hermit's `ControlClient` and the `:7070` router agree, including the 409 `sandbox_busy` outcome, and the local
//! router hands the received descriptors to the server.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::unused_async_trait_impl)]

use std::os::fd::OwnedFd;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use hermit_api::server::Answer;
use serde::de::DeserializeOwned;
use supervisor_api::client::ControlClient;
use supervisor_api::control::{
    Health, Outcome, PrepareAnswer, PrepareRequest, ReleaseAnswer, ReleaseRequest, Resumed, RunSummary, StateReport,
    busy, not_ready,
};
use supervisor_api::local::{ReceivedFds, SendUserFileReply, SendUserFileRequest};
use supervisor_api::server::{ControlServer, Ctx, LocalServer, control_router, local_router};
use supervisor_api::{ActivityState, ExecRequest, ExecResult, SecretValue, control_token_matches, sha256_hex};

fn fx<T: DeserializeOwned>(name: &str) -> T {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let p = if let Some(name) = name.strip_prefix("hermit:") {
        dir.join("../hermit-api/fixtures").join(format!("{name}.json"))
    } else {
        dir.join("fixtures").join(format!("{name}.json"))
    };
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

const CONTROL_TOKEN: &str = "hct_test-control-token";

struct Sup {
    described: AtomicBool,
    hashes: Vec<String>,
}

impl Sup {
    fn check(&self, ctx: &Ctx) -> Answer<()> {
        if !self.described.load(Ordering::SeqCst) {
            return Err(not_ready());
        }
        match ctx.bearer() {
            Some(t) if control_token_matches(t, &self.hashes) => {}
            _ => return Err(aviary_core::http::ApiError::unauthenticated()),
        }
        assert_eq!(ctx.protocol(), Some(("supervisor-control", 1)));
        Ok(())
    }
}

impl ControlServer for Sup {
    async fn health(&self, ctx: Ctx) -> Answer<Health> {
        self.check(&ctx)?;
        Ok(fx("health"))
    }
    async fn state(&self, ctx: Ctx) -> Answer<StateReport> {
        self.check(&ctx)?;
        Ok(fx("state"))
    }
    async fn prepare(&self, ctx: Ctx, body: PrepareRequest) -> Answer<PrepareAnswer> {
        self.check(&ctx)?;
        if body.force {
            Ok(fx("prepare-answer"))
        } else {
            Err(busy(RunSummary {
                state: ActivityState::Running,
                turn_id: None,
                tasks: 0,
            }))
        }
    }
    async fn resumed(&self, ctx: Ctx, body: Resumed) -> Answer<()> {
        self.check(&ctx)?;
        assert_eq!(body, fx("resumed"));
        Ok(())
    }
    async fn release(&self, ctx: Ctx, body: ReleaseRequest) -> Answer<ReleaseAnswer> {
        self.check(&ctx)?;
        if body.force {
            Ok(fx("release-answer"))
        } else {
            Err(busy(
                serde_json::from_value(fx::<serde_json::Value>("error-busy")["run"].clone()).unwrap(),
            ))
        }
    }
    async fn exec(&self, ctx: Ctx, body: ExecRequest) -> Answer<ExecResult> {
        self.check(&ctx)?;
        assert_eq!(body, fx("hermit:exec-request"));
        Ok(fx("hermit:exec-result"))
    }
}

async fn serve(router: axum::Router) -> String {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    format!("http://{addr}")
}

#[tokio::test]
async fn control_client_and_router_agree() {
    let sup = Arc::new(Sup {
        described: AtomicBool::new(false),
        hashes: vec![sha256_hex(CONTROL_TOKEN)],
    });
    let base = serve(control_router(sup.clone())).await;
    let c = ControlClient::new(reqwest::Client::new(), base.clone(), CONTROL_TOKEN).request_id("r1");

    // before the descriptor: 503 unavailable, retry-after 1
    let e = c.health().await.unwrap_err();
    let supervisor_api::client::ClientError::Api(r) = e else {
        panic!("{e}")
    };
    assert_eq!(
        (r.status, r.retry_after, r.body.code.as_str()),
        (503, Some(1), "unavailable")
    );
    sup.described.store(true, Ordering::SeqCst);

    assert_eq!(c.health().await.unwrap(), fx::<Health>("health"));
    assert_eq!(c.state().await.unwrap(), fx::<StateReport>("state"));
    assert_eq!(c.prepare(&fx("prepare-pause")).await.unwrap(), Outcome::Done);
    let busy_run = RunSummary {
        state: ActivityState::Running,
        turn_id: None,
        tasks: 0,
    };
    assert_eq!(
        c.prepare(&PrepareRequest::flush()).await.unwrap(),
        Outcome::Busy(busy_run)
    );
    c.resumed(&fx("resumed")).await.unwrap();
    assert_eq!(c.release(&fx("release-host-lost")).await.unwrap(), Outcome::Done);
    let Outcome::Busy(run) = c.release(&fx("release-idle")).await.unwrap() else {
        panic!("not busy")
    };
    assert_eq!(run.state, ActivityState::Running);
    assert_eq!(
        c.exec(&fx("hermit:exec-request")).await.unwrap(),
        fx::<ExecResult>("hermit:exec-result")
    );

    // a wrong token is 401 with www-authenticate
    let wrong = ControlClient::new(reqwest::Client::new(), base.clone(), "hct_other");
    let e = wrong.health().await.unwrap_err();
    assert_eq!(e.status(), Some(401));
    let res = reqwest::get(format!("{base}/v1/nothing")).await.unwrap();
    assert_eq!(res.status(), 404);
    assert_eq!(res.headers()["cache-control"], "no-store");
}

struct Local;

impl LocalServer for Local {
    async fn send_user_file(&self, _: Ctx, body: SendUserFileRequest, fds: Vec<OwnedFd>) -> Answer<SendUserFileReply> {
        assert_eq!(body, fx("send-user-file"));
        Ok(SendUserFileReply {
            sent: u32::try_from(fds.len()).unwrap(),
        })
    }
    async fn secret(&self, _: Ctx, key: String) -> Answer<SecretValue> {
        if key == "EXAMPLE_TOKEN" {
            Ok(fx("secret"))
        } else {
            Err(hermit_api::error::not_found(
                hermit_api::error::codes::NOT_GRANTED,
                "key not granted",
            ))
        }
    }
}

#[tokio::test]
async fn the_local_router_hands_over_descriptors() {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    let app = local_router(Arc::new(Local));
    let a = std::fs::File::open("/dev/null").unwrap();
    let b = std::fs::File::open("/dev/null").unwrap();
    let body = serde_json::to_vec(&fx::<serde_json::Value>("send-user-file")).unwrap();
    let mut req = Request::post(supervisor_api::local::SEND_USER_FILE)
        .body(Body::from(body))
        .unwrap();
    req.extensions_mut().insert(ReceivedFds::new(vec![a.into(), b.into()]));
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(res.status(), 200);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let reply: SendUserFileReply = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(reply, fx("send-user-file-reply"));

    let req = Request::get("/v1/secrets/OTHER").body(Body::empty()).unwrap();
    let res = app.oneshot(req).await.unwrap();
    assert_eq!(res.status(), 404);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["code"], "not_granted");
}

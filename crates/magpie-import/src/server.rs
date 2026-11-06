//! The import Deployment's long-running commands (cutover 7.1, 7.5, 7.9; platform 3.7):
//! - `handover`: the hand-over loop ([`crate::handover`]) as the leader of `magpie-import.leader`, a pass every 5 s;
//!   the ops port (9090) adds `GET /status` (the status document) to `/metrics`, `/readyz`, `/healthz`;
//!   between two passes it lets the lock go when a one-shot command waits for it ([`crate::lock::waiter_present`]),
//!   and bids again at once (the waiter was granted the lock as it was let go, so the bid wins only after it ends);
//! - `export-server`: the writer sidecar's API on 8080 (with its ops routes there: the pod's 9090 is the hand-over's), `POST /v1/ingest-upload` (magpie's synchronous ingest of one
//!   old asset on a miss, [`magpie::legacy::ingest`]) for the caller magpie (`MAGPIE_IMPORT_ACCEPT_MAGPIE`); and, on
//!   the pod-local [`EXPORT_ADDR`] only (the import pod's own containers, no token), `POST /v1/export`: the reverse
//!   export (cutover 14.3) of reverse-fenced conversations for the reverse hand-over ([`crate::reverse`]).

use std::process::ExitCode;
use std::sync::Arc;

use aviary_core::auth::{Caller, Callers};
use aviary_core::{drain, env, ops};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use magpie::legacy::ingest::{IngestRequest, ROUTE};

use crate::Importer;
use crate::handover::Handover;

/// The export server's address (Service `magpie-import` 80 → 8080).
pub const API_ADDR: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 8080);

/// The export route's pod-local address ([`crate::reverse::EXPORT_URL`]).
pub const EXPORT_ADDR: std::net::SocketAddr =
    std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 8081);

/// `magpie import handover`.
pub async fn handover(imp: Importer, ops: ops::Ops) -> ExitCode {
    let kube = match crate::kube::Kube::from_env() {
        Ok(k) => k,
        Err(e) => {
            tracing::error!(error = %e, "the old Kubernetes API is not configured");
            return ExitCode::FAILURE;
        }
    };
    let token = env::required("MAGPIE_IMPORT_TOKEN_UI");
    let mut h = Handover::new(kube, token);
    let status = h.status.clone();
    let imp = Arc::new(imp);
    let st_imp = imp.clone();
    let router = ops.router().merge(Router::new().route(
        "/status",
        get(move || {
            let (status, imp) = (status.clone(), st_imp.clone());
            async move {
                let base = crate::control::status(&imp).await.ok();
                let lp = status.read().await.clone();
                Json(serde_json::json!({ "control": base, "loop": lp })).into_response()
            }
        }),
    ));
    let listener = match tokio::net::TcpListener::bind(ops::ADDR).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen on the ops port");
            return ExitCode::FAILURE;
        }
    };
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "ops server ended");
        }
    });
    let mut signals = match drain::Signals::os() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot install signal handlers");
            return ExitCode::FAILURE;
        }
    };
    ops.readiness().started();
    let mut leader = crate::lock::leader(&imp.magpie.pool);
    loop {
        let next = tokio::time::Instant::now() + crate::handover::CADENCE;
        if leader.confirm().await {
            let outcome;
            (h, outcome) = match pass_apart(h, imp.clone()).await {
                Ok(x) => x,
                Err(e) => {
                    tracing::error!(error = %e, "the pass's thread ended");
                    leader.release().await;
                    return ExitCode::FAILURE;
                }
            };
            match outcome {
                Ok(p) => tracing::debug!(ms = p.ms, changed = p.changed, released = p.released, "pass"),
                Err(e) => {
                    tracing::warn!(error = %e, "the pass stopped");
                    h.status.write().await.pass.error = Some(e.to_string());
                }
            }
            // a one-shot command waits (`allow-handover`, `reverse`, `hold`, …): it gets the lock as it is let go
            if matches!(crate::lock::waiter_present(&imp.magpie.pool).await, Ok(true)) {
                tracing::info!("a one-shot command waits for the lock: letting it go");
                leader.release().await;
                h.status.write().await.leader = false;
                leader = crate::lock::leader(&imp.magpie.pool);
            }
        } else {
            h.status.write().await.leader = false;
        }
        tokio::select! {
            () = tokio::time::sleep_until(next) => {}
            s = signals.recv() => {
                tracing::info!(signal = ?s, "draining");
                ops.readiness().set_draining();
                leader.release().await;
                return ExitCode::SUCCESS;
            }
        }
    }
}

/// Runs one pass on a blocking-pool thread (`Handle::block_on` there), so the pass's synchronous work (the `JuiceFS`
/// tree walks, reads and hashes through FUSE, a 57 s pass on 2026-11-06) never holds a runtime worker. The pod's
/// one CPU gives the runtime one worker; a pass on it starved the leader connection's 1 s check, the listener's
/// 5 s ping and new pool connections, and the leader lost the lock about every 10 min ("leader: connection check
/// timed out"). The pass's futures still run on this runtime's drivers (timers, sockets, spawned connection
/// tasks), which the free worker keeps polling. A `current_thread` runtime (no free worker) runs the pass inline.
async fn pass_apart(
    mut h: Handover,
    imp: Arc<Importer>,
) -> Result<(Handover, crate::Result<crate::handover::PassInfo>), tokio::task::JoinError> {
    let rt = tokio::runtime::Handle::current();
    if rt.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
        // boxed: the pass's future inline here overflows the layout query depth of `cli`'s block
        let r = Box::pin(h.pass(&imp)).await;
        return Ok((h, r));
    }
    tokio::task::spawn_blocking(move || {
        let r = rt.block_on(h.pass(&imp));
        (h, r)
    })
    .await
}

#[derive(Clone)]
struct ExportState {
    imp: Arc<Importer>,
    callers: Callers,
}

async fn ingest_upload(State(s): State<ExportState>, headers: HeaderMap, Json(req): Json<IngestRequest>) -> Response {
    if s.callers.identify_headers(&headers) != Some(Caller::Magpie) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let key = aviary_core::log::person_key(&req.person);
    match crate::uploads::one(&s.imp, &req.person, &key, &req.legacy_path).await {
        Ok(true) => StatusCode::OK.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            tracing::warn!(person_key = %key, error = %e, "the synchronous ingest failed");
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn export(State(imp): State<Arc<Importer>>, Json(req): Json<crate::reverse::ExportRequest>) -> Response {
    match crate::export::export(&imp, None, &req.conversations, false).await {
        Ok(x) => Json(x).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "the export failed");
            (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response()
        }
    }
}

/// The pod-local export route (`POST /v1/export`): the reverse hand-over's export of reverse-fenced conversations.
pub fn export_router(imp: Arc<Importer>) -> Router {
    Router::new()
        .route(crate::reverse::EXPORT_ROUTE, post(export))
        .with_state(imp)
}

/// `magpie import export-server`.
pub async fn export_server(imp: Importer, ops: ops::Ops) -> ExitCode {
    let imp = Arc::new(imp);
    let state = ExportState {
        imp: imp.clone(),
        callers: Callers::from_env("MAGPIE_IMPORT", &[Caller::Magpie]),
    };
    let local = match tokio::net::TcpListener::bind(EXPORT_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen on the pod-local export port");
            return ExitCode::FAILURE;
        }
    };
    let local_app = export_router(imp);
    tokio::spawn(async move {
        if let Err(e) = axum::serve(local, local_app).await {
            tracing::error!(error = %e, "the export route ended");
        }
    });
    // the ops routes (/readyz, /healthz, /metrics) on the API port: the hand-over container of the same pod holds 9090
    let app = Router::new()
        .route(ROUTE, post(ingest_upload))
        .with_state(state)
        .merge(ops.router());
    let listener = match tokio::net::TcpListener::bind(API_ADDR).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(error = %e, "cannot listen on 8080");
            return ExitCode::FAILURE;
        }
    };
    let mut signals = match drain::Signals::os() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "cannot install signal handlers");
            return ExitCode::FAILURE;
        }
    };
    ops.readiness().started();
    let readiness = ops.readiness().clone();
    let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
        let s = signals.recv().await;
        tracing::info!(signal = ?s, "draining");
        readiness.set_draining();
    });
    if let Err(e) = serve.await {
        tracing::error!(error = %e, "the export server ended");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

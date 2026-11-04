//! The import Deployment's long-running commands (cutover 7.1, 7.5, 7.9; platform 3.7):
//! - `handover`: the hand-over loop ([`crate::handover`]) as the leader of `magpie-import.leader`, a pass every 5 s;
//!   the ops port (9090) adds `GET /status` (the status document) to `/metrics`, `/readyz`, `/healthz`;
//! - `export-server`: the writer sidecar's API on 8080, `POST /v1/ingest-upload` (magpie's synchronous ingest of one
//!   old asset on a miss, [`magpie::legacy::ingest`]) for the caller magpie (`MAGPIE_IMPORT_ACCEPT_MAGPIE`).

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
    let leader = crate::lock::leader(&imp.magpie.pool);
    loop {
        let next = tokio::time::Instant::now() + crate::handover::CADENCE;
        if leader.confirm().await {
            match h.pass(&imp).await {
                Ok(p) => tracing::debug!(ms = p.ms, changed = p.changed, released = p.released, "pass"),
                Err(e) => {
                    tracing::warn!(error = %e, "the pass stopped");
                    h.status.write().await.pass.error = Some(e.to_string());
                }
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

/// `magpie import export-server`.
pub async fn export_server(imp: Importer, ops: ops::Ops) -> ExitCode {
    let state = ExportState {
        imp: Arc::new(imp),
        callers: Callers::from_env("MAGPIE_IMPORT", &[Caller::Magpie]),
    };
    if let Err(e) = ops.spawn(ops::ADDR).await {
        tracing::error!(error = %e, "cannot listen on the ops port");
        return ExitCode::FAILURE;
    }
    let app = Router::new().route(ROUTE, post(ingest_upload)).with_state(state);
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

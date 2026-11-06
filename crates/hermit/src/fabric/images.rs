//! Image digests (hermit 13.14): `sandbox.image` is pinned by digest, so the digest is the pin and hermit needs no
//! registry lookup. A host that just became active gets `POST /v1/images {digest}` (the warm-up), in the background;
//! the gateway, the image server of hosts without registry access, gets `POST /v1/images {ref, digest}` from the
//! scaler's pass until it holds the configured digest (it serves only images it holds: without this a new
//! `sandbox.image` failed every start on a host that lacked it, 404 from the gateway, 2026-11-06);
//! running sandboxes keep their image and new ones use the configured one. sandboxd's 4-slot pull table and 20 min
//! wait are its own.

use std::sync::PoisonError;

use crate::fabric::gvisor::{Gvisor, bare_digest};
use crate::fabric::hosts::HostRow;

/// Warms the configured image on a host that just became active (best effort, not awaited).
pub fn warm(g: &Gvisor, h: &HostRow, digest: &str) {
    let Ok(c) = g.client(h) else {
        return;
    };
    let (name, digest) = (h.name.clone(), bare_digest(digest).to_owned());
    tokio::spawn(async move {
        let req = sandboxd_api::EnsureImage {
            reference: None,
            digest: Some(digest),
        };
        match c.ensure_image(&req).await {
            Ok(d) => tracing::info!(host = %name, digest = %d.digest, "image warmed"),
            Err(e) => tracing::warn!(host = %name, error = %e, "image warm-up (the first create pulls it)"),
        }
    });
}

/// Makes the gateway pull the configured image (`reference` = `sandbox.image`, pinned by digest) once per digest, in
/// the background; a failure clears the mark so the next pass asks again. sandboxd answers at once when it holds it.
pub fn warm_gateway(g: &Gvisor, h: &HostRow, reference: &str, digest: &str) {
    let digest = bare_digest(digest).to_owned();
    {
        let mut held = g.gateway_image.lock().unwrap_or_else(PoisonError::into_inner);
        if held.as_deref() == Some(digest.as_str()) {
            return;
        }
        *held = Some(digest.clone());
    }
    let Ok(c) = g.client(h) else {
        *g.gateway_image.lock().unwrap_or_else(PoisonError::into_inner) = None;
        return;
    };
    let (mark, reference) = (g.gateway_image.clone(), reference.to_owned());
    tokio::spawn(async move {
        let req = sandboxd_api::EnsureImage {
            reference: Some(reference),
            digest: Some(digest.clone()),
        };
        match c.ensure_image(&req).await {
            Ok(d) => tracing::info!(digest = %d.digest, "gateway holds the sandbox image"),
            Err(e) => {
                tracing::warn!(error = %e, "gateway image pull (asked again next pass)");
                let mut held = mark.lock().unwrap_or_else(PoisonError::into_inner);
                if held.as_deref() == Some(digest.as_str()) {
                    *held = None;
                }
            }
        }
    });
}

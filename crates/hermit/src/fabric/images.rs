//! Image digests (hermit 13.14): `sandbox.image` is pinned by digest, so the digest is the pin and hermit needs no
//! registry lookup. A host that just became active gets `POST /v1/images {digest}` (the warm-up), in the background;
//! the gateway, the image server of hosts without registry access, gets `POST /v1/images {ref, digest}` from the
//! scaler's pass whenever its answer does not list the configured digest (it serves only images it holds, and its
//! store is a cache: a new pod starts without any). Running sandboxes keep their image; new ones use the configured
//! one once the gateway holds it, and the image of the last ready sandbox until then (`Fabric::image_ready`: a new
//! `sandbox.image` the gateway could not fetch failed every start, 2026-11-06, 2026-11-12, 2026-11-16).
//! With an image bucket (`fabric.aws.imageBucket`) the gateway serves no image and is never asked: hosts fetch from
//! the bucket, and "hosts can get it" is "it is published there". sandboxd's 4-slot pull table and 20 min
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

/// Makes the gateway pull the configured image (`reference` = `sandbox.image`, pinned by digest), in the background,
/// one ask at a time. Called on every pass whose answer from the gateway does not list the image: a gateway's store
/// is a cache, so a gateway that comes back without the image is asked again, and a failed pull is asked again on
/// the next pass.
pub fn warm_gateway(g: &Gvisor, h: &HostRow, reference: &str, digest: &str) {
    let digest = bare_digest(digest).to_owned();
    {
        let mut asking = g.gateway_image.lock().unwrap_or_else(PoisonError::into_inner);
        if asking.as_deref() == Some(digest.as_str()) {
            return;
        }
        *asking = Some(digest.clone());
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
            Err(e) => tracing::warn!(error = %e, "gateway image pull (asked again next pass)"),
        }
        // the ask is over: whether the gateway holds the image is read from its next answer
        let mut asking = mark.lock().unwrap_or_else(PoisonError::into_inner);
        if asking.as_deref() == Some(digest.as_str()) {
            *asking = None;
        }
    });
}

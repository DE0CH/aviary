//! Image digests (hermit 13.14): `sandbox.image` is pinned by digest, so the digest is the pin and hermit needs no
//! registry lookup. A host that just became active gets `POST /v1/images {digest}` (the warm-up), in the background;
//! running sandboxes keep their image and new ones use the configured one. sandboxd's 4-slot pull table and 20 min
//! wait are its own.

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

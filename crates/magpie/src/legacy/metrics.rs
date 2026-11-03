//! The window's metrics (DESIGN 9.9): `magpie_fence_seconds` (fence to release, p99 <= 0.3 s and max <= 3 s),
//! `magpie_unknown_upload_total{legacy}` (`legacy="true"` is a zero counter), `magpie_conversation_migrating_total`
//! (409 `route: "legacy"` answers: only a stale pigeon owner cache gets one, re-routed, never shown) and
//! `magpie_legacy_path_total` (old asset paths sends carried: A7 waits for 30 days of none).

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Legacy {
    legacy: String,
}

/// The handles. Cheap to clone; a replica that registers nothing (a test) counts into handles nobody reads.
#[derive(Debug, Clone)]
pub struct Metrics {
    fence: Histogram,
    unknown_upload: Family<Legacy, Counter>,
    migrating: Counter,
    legacy_paths: Counter,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            fence: Histogram::new([0.01, 0.05, 0.1, 0.2, 0.3, 0.5, 1.0, 2.0, 3.0]),
            unknown_upload: Family::default(),
            migrating: Counter::default(),
            legacy_paths: Counter::default(),
        }
    }
}

impl Metrics {
    /// Registers every metric on `registry` (whose prefix is `magpie_`).
    pub fn register(registry: &mut Registry) -> Self {
        let m = Self::default();
        registry.register(
            "fence_seconds",
            "Fence to release of a conversation's hand-over (DESIGN 9.4)",
            m.fence.clone(),
        );
        registry.register(
            "unknown_upload",
            "Sends refused 400 unknown_upload, by whether the path was an old asset path",
            m.unknown_upload.clone(),
        );
        registry.register(
            "conversation_migrating",
            "409 conversation_migrating answers (route legacy) to native mutations",
            m.migrating.clone(),
        );
        registry.register(
            "legacy_path",
            "Old asset paths carried by sends (A7 waits for 30 days without one)",
            m.legacy_paths.clone(),
        );
        m
    }

    /// A fence held this long before its release.
    pub fn fence_released(&self, seconds: f64) {
        self.fence.observe(seconds);
    }

    /// A send refused with `unknown_upload`.
    pub fn unknown_upload(&self, legacy: bool) {
        self.unknown_upload
            .get_or_create(&Legacy {
                legacy: legacy.to_string(),
            })
            .inc();
    }

    /// A native mutation answered 409 `route: "legacy"`.
    pub fn migrating(&self) {
        self.migrating.inc();
    }

    /// A send carried an old asset path.
    pub fn legacy_path(&self) {
        self.legacy_paths.inc();
    }
}

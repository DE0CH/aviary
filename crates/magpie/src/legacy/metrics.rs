//! The window's metrics (DESIGN 9.9): `magpie_fence_seconds` (fence to release, p99 <= 0.3 s and max <= 3 s),
//! `magpie_unknown_upload_total{legacy}` (`legacy="true"` is a zero counter), `magpie_conversation_migrating_total`
//! (409 `route: "legacy"` answers: only a stale pigeon owner cache gets one, re-routed, never shown) and
//! `magpie_legacy_path_total` (old asset paths sends carried: A7 waits for 30 days of none) and
//! `magpie_import_verify_mismatch_total` (DESIGN 9.9's `import_verify_mismatch_total`, a zero counter: files whose
//! reconstruction still differed from the old source after verification re-ingested them; counted by the importer,
//! which registers these metrics on its own `:9090/metrics`, and never by magpie itself).

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
    verify_mismatch: Counter,
}

impl Default for Metrics {
    fn default() -> Self {
        Self {
            fence: Histogram::new([0.01, 0.05, 0.1, 0.2, 0.3, 0.5, 1.0, 2.0, 3.0]),
            unknown_upload: Family::default(),
            migrating: Counter::default(),
            legacy_paths: Counter::default(),
            verify_mismatch: Counter::default(),
        }
    }
}

impl Metrics {
    /// Registers every metric on `registry` (whose prefix is `magpie_`).
    pub fn register(registry: &mut Registry) -> Self {
        let m = Self::default();
        // both label values exist from the start, so `legacy="true"` (a zero counter) is scraped at 0 rather than
        // absent (testing/switch/zero-counters.txt)
        for legacy in [true, false] {
            drop(m.unknown_upload.get_or_create(&Legacy {
                legacy: legacy.to_string(),
            }));
        }
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
        registry.register(
            "import_verify_mismatch",
            "Files the importer's verification found unequal to the old source after re-ingesting them (zero)",
            m.verify_mismatch.clone(),
        );
        m
    }

    /// `magpie_import_verify_mismatch_total` as counted so far (tests).
    #[must_use]
    pub fn verify_mismatches(&self) -> u64 {
        self.verify_mismatch.get()
    }

    /// Verification left `files` files unequal to the old source.
    pub fn verify_mismatch(&self, files: usize) {
        self.verify_mismatch.inc_by(u64::try_from(files).unwrap_or(u64::MAX));
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

//! The legacy-path set's metrics: `magpie_unknown_upload_total{legacy}` (sends refused 400 `unknown_upload`, by
//! whether the path was an old asset path) and `magpie_legacy_path_total` (old asset paths sends carried: A7 waits for
//! 30 days of none, cutover 10.7).

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::registry::Registry;

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Legacy {
    legacy: String,
}

/// The handles. Cheap to clone; a replica that registers nothing (a test) counts into handles nobody reads.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    unknown_upload: Family<Legacy, Counter>,
    legacy_paths: Counter,
}

impl Metrics {
    /// Registers every metric on `registry` (whose prefix is `magpie_`).
    pub fn register(registry: &mut Registry) -> Self {
        let m = Self::default();
        // both label values exist from the start, so either is scraped at 0 rather than absent
        for legacy in [true, false] {
            drop(m.unknown_upload.get_or_create(&Legacy {
                legacy: legacy.to_string(),
            }));
        }
        registry.register(
            "unknown_upload",
            "Sends refused 400 unknown_upload, by whether the path was an old asset path",
            m.unknown_upload.clone(),
        );
        registry.register(
            "legacy_path",
            "Old asset paths carried by sends (A7 waits for 30 days without one)",
            m.legacy_paths.clone(),
        );
        m
    }

    /// A send refused with `unknown_upload`.
    pub fn unknown_upload(&self, legacy: bool) {
        self.unknown_upload
            .get_or_create(&Legacy {
                legacy: legacy.to_string(),
            })
            .inc();
    }

    /// A send carried an old asset path.
    pub fn legacy_path(&self) {
        self.legacy_paths.inc();
    }
}

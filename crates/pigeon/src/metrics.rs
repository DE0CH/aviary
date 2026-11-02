//! pigeon's metrics of spec 19 that the HTTP surface owns (the socket package adds its own).

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::histogram::Histogram;
use prometheus_client::registry::Registry;

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct RouteStatus {
    route: String,
    status: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Result_ {
    result: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Team {
    team: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct ViewAs {
    viewas: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Code {
    code: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Hello {
    protocol: String,
    client_type: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Reason {
    reason: String,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq, EncodeLabelSet)]
struct Kind {
    kind: String,
}

/// The handles. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Metrics {
    http: Family<RouteStatus, Counter>,
    identity: Family<Result_, Counter>,
    stale: Counter,
    upload_bytes: Counter,
    jwks: Family<Team, Counter>,
    drain_transfer: Histogram,
    sockets: Family<ViewAs, prometheus_client::metrics::gauge::Gauge>,
    closes: Family<Code, Counter>,
    hellos: Family<Hello, Counter>,
    dropped: Family<Reason, Counter>,
    rpc: Family<Kind, Histogram>,
}

impl Metrics {
    /// Registers every metric on `registry` (whose prefix is `pigeon_`).
    pub fn register(registry: &mut Registry) -> Self {
        let m = Self {
            http: Family::default(),
            identity: Family::default(),
            stale: Counter::default(),
            upload_bytes: Counter::default(),
            jwks: Family::default(),
            drain_transfer: Histogram::new([1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0]),
            sockets: Family::default(),
            closes: Family::default(),
            hellos: Family::default(),
            dropped: Family::default(),
            rpc: Family::new_with_constructor(|| {
                Histogram::new([0.005, 0.02, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 15.0, 30.0])
            }),
        };
        registry.register("sockets", "Open sockets", m.sockets.clone());
        registry.register("socket_closes", "Socket closes by close code", m.closes.clone());
        registry.register(
            "hello",
            "Socket handshakes by protocol and client type (spec 1.2)",
            m.hellos.clone(),
        );
        registry.register("frames_dropped", "Inbound frames dropped by reason", m.dropped.clone());
        registry.register("rpc_seconds", "Socket request handling time by type", m.rpc.clone());
        registry.register(
            "http_requests",
            "Answers on port 8080 by route and status",
            m.http.clone(),
        );
        registry.register(
            "identity",
            "Request identities by result (spec 4.6)",
            m.identity.clone(),
        );
        registry.register(
            "hermit_stale_answers",
            "People answers served from the cache while hermit failed",
            m.stale.clone(),
        );
        registry.register("upload_bytes", "File bytes streamed to magpie", m.upload_bytes.clone());
        registry.register("jwks_fetches", "Access key set fetches by team", m.jwks.clone());
        registry.register(
            "drain_transfer_seconds",
            "How long the drain waited for transfers",
            m.drain_transfer.clone(),
        );
        m
    }

    /// A metrics set registered nowhere (tests of single modules).
    #[must_use]
    pub fn detached() -> Self {
        Self::register(&mut Registry::default())
    }

    pub(crate) fn http(&self, route: &str, status: u16) {
        self.http
            .get_or_create(&RouteStatus {
                route: route.to_owned(),
                status: status.to_string(),
            })
            .inc();
    }

    pub(crate) fn identity(&self, result: &str) {
        self.identity
            .get_or_create(&Result_ {
                result: result.to_owned(),
            })
            .inc();
    }

    pub(crate) fn hermit_stale(&self) {
        self.stale.inc();
    }

    pub(crate) fn upload_bytes(&self, n: u64) {
        self.upload_bytes.inc_by(n);
    }

    pub(crate) fn jwks_fetch(&self, team: &str) {
        self.jwks.get_or_create(&Team { team: team.to_owned() }).inc();
    }

    pub(crate) fn socket_opened(&self, viewas: bool) {
        self.sockets
            .get_or_create(&ViewAs {
                viewas: viewas.to_string(),
            })
            .inc();
    }

    pub(crate) fn socket_closed(&self, viewas: bool, code: u16) {
        self.sockets
            .get_or_create(&ViewAs {
                viewas: viewas.to_string(),
            })
            .dec();
        self.closes.get_or_create(&Code { code: code.to_string() }).inc();
    }

    pub(crate) fn hello(&self, protocol: &str, client_type: &str) {
        // client types are the app's own small set; anything else is counted as "other"
        let client_type = match client_type {
            "browser" | "mobile" | "desktop" | "cli" => client_type,
            _ => "other",
        };
        self.hellos
            .get_or_create(&Hello {
                protocol: protocol.chars().take(8).collect(),
                client_type: client_type.to_owned(),
            })
            .inc();
    }

    pub(crate) fn frame_dropped(&self, reason: &str) {
        self.dropped
            .get_or_create(&Reason {
                reason: reason.to_owned(),
            })
            .inc();
    }

    pub(crate) fn rpc(&self, kind: &str, secs: f64) {
        self.rpc.get_or_create(&Kind { kind: kind.to_owned() }).observe(secs);
    }

    pub(crate) fn drain_transfer(&self, secs: f64) {
        self.drain_transfer.observe(secs);
    }
}

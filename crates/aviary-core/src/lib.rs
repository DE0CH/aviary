//! The service runtime shared by every aviary binary (platform spec 3.1-3.9, 4.3, 15.1). Nothing in it knows a
//! deployment: every value a service needs comes from its environment through [`env`], and every behaviour is code.
//!
//! - [`env`]: the only place the process environment is read; a missing or empty value aborts start.
//! - [`log`]: one JSON object per line, every string field passed through [`log::redact`].
//! - [`ops`]: the `:9090` server (`/healthz`, `/readyz`, `/version`, `/metrics`) and readiness gates.
//! - [`drain`]: SIGTERM/SIGINT, ordered drain hooks with per-hook deadlines, a second signal exits at once.
//! - [`auth`]: pair-token callers (`<CALLEE>_ACCEPT_<CALLER>`) and per-router guards.
//! - [`http`]: the error envelope, the `Aviary-*` headers, request ids, response defaults, the outbound client and
//!   the retry helper.
//! - [`version`]: the build facts and protocol numbers.
//!
//! A service's `main` in outline:
//!
//! ```no_run
//! # async fn example() -> std::io::Result<()> {
//! use aviary_core::{auth, drain, http, log, ops, version};
//! use std::time::Duration;
//!
//! let build = version::Build::new("magpie").surface("magpie-person", 1).surface("magpie-sandbox", 2);
//! log::init(&build, Some(aviary_core::env::field("POD_NAME")), log::Output::Stdout);
//! let callers = auth::Callers::from_env("MAGPIE", &[auth::Caller::Pigeon, auth::Caller::Admin]);
//! let registry = ops::registry(&build);
//! let ops = ops::Ops::new(build, registry);
//! ops.spawn(ops::ADDR).await?;
//! let started = ops.readiness().gate("migrations");
//! // … migrations, the API listener …
//! started.open();
//! let mut drain = drain::Drain::new(ops.readiness().clone());
//! drain.hook("close listener", Duration::from_secs(5), || async {});
//! let outcome = drain.run(&mut drain::Signals::os()?).await;
//! drain::exit(outcome)
//! # }
//! ```

pub mod auth;
pub mod drain;
pub mod env;
pub mod http;
pub mod log;
pub mod ops;
pub mod version;

//! The move's input (DESIGN 9.3): per person, the last warm call and the last upload this replica received. After T0
//! every HTTP call of an old tab reaches pigeon, so the runner moves a person's old sockets only when no replica saw
//! either in the last 60 s (the composer sends a warm call on the first keystroke and then at most every 30 s).
//! `GET /internal/switch/activity` on the ops port, which the edge never reaches (the runner reads each replica
//! through a port-forward).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::get;
use magpie_api::Timestamp;
use serde_json::{Map, Value, json};

use crate::App;

/// The route.
pub const PATH: &str = "/internal/switch/activity";
/// Entries older than this are dropped (the runner looks back 60 s).
const KEEP: Duration = Duration::from_hours(1);

#[derive(Debug, Clone, Copy, Default)]
struct Seen {
    warm: Option<time::OffsetDateTime>,
    upload: Option<time::OffsetDateTime>,
}

/// The per-person record. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Activity(Arc<Mutex<HashMap<String, Seen>>>);

impl Activity {
    fn with(&self, person: &str, f: impl FnOnce(&mut Seen)) {
        let mut m = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = time::OffsetDateTime::now_utc();
        if m.len() > 20_000 {
            m.retain(|_, s| {
                let last = s.warm.max(s.upload);
                last.is_some_and(|t| now - t < KEEP)
            });
        }
        f(m.entry(person.to_owned()).or_default());
    }

    /// A warm call of `person`.
    pub fn warm(&self, person: &str) {
        self.with(person, |s| s.warm = Some(time::OffsetDateTime::now_utc()));
    }

    /// An upload of `person`.
    pub fn upload(&self, person: &str) {
        self.with(person, |s| s.upload = Some(time::OffsetDateTime::now_utc()));
    }

    /// `{"generatedAt", "persons": {email: {"lastWarm", "lastUpload"}}}`, entries of the last hour.
    #[must_use]
    pub fn report(&self) -> Value {
        let now = time::OffsetDateTime::now_utc();
        let m = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut persons = Map::new();
        for (p, s) in m.iter() {
            let last = s.warm.max(s.upload);
            if last.is_none_or(|t| now - t >= KEEP) {
                continue;
            }
            persons.insert(
                p.clone(),
                json!({"lastWarm": s.warm.map(Timestamp), "lastUpload": s.upload.map(Timestamp)}),
            );
        }
        json!({"generatedAt": Timestamp(now), "persons": persons})
    }
}

async fn activity(State(app): State<App>) -> Response {
    crate::http::json(StatusCode::OK, &app.legacy().0.activity.report())
}

/// The ops port's internal routes.
pub fn router(app: App) -> Router {
    Router::new().route(PATH, get(activity)).with_state(app)
}

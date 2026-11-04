//! The move's input (DESIGN 9.3): per person, the last warm call and the last upload this replica received. After T0
//! every HTTP call of an old tab reaches pigeon, so the runner moves a person's old sockets only when no replica saw
//! either in the last 60 s (the composer sends a warm call on the first keystroke and then at most every 30 s). An
//! upload counts from its start to its end: while one is in progress its person's `lastUpload` is the report's own
//! time (and `uploading` counts them), so a slow upload never ages out of the look-back mid-way.
//! The record is this replica's memory since it started: the report carries `startedAt` and `complete` (`false`
//! while the replica has been up less than [`LOOKBACK`]), and a runner treats an incomplete report as "activity
//! unknown" and moves nobody on it.
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
/// How far back the runner looks: a replica up for less than this cannot say who is typing.
pub const LOOKBACK: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, Default)]
struct Seen {
    warm: Option<time::OffsetDateTime>,
    upload: Option<time::OffsetDateTime>,
    /// Uploads in progress.
    uploading: usize,
}

/// An upload in progress ([`Activity::uploading`]): its end is stamped too.
#[derive(Debug)]
pub struct Uploading(Activity, String);

impl Drop for Uploading {
    fn drop(&mut self) {
        self.0.with(&self.1, |s| {
            s.uploading = s.uploading.saturating_sub(1);
            s.upload = Some(time::OffsetDateTime::now_utc());
        });
    }
}

/// The per-person record. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Activity(
    Arc<Mutex<HashMap<String, Seen>>>,
    time::OffsetDateTime,
    std::time::Instant,
);

impl Default for Activity {
    fn default() -> Self {
        Self(
            Arc::default(),
            time::OffsetDateTime::now_utc(),
            std::time::Instant::now(),
        )
    }
}

impl Activity {
    fn with(&self, person: &str, f: impl FnOnce(&mut Seen)) {
        let mut m = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = time::OffsetDateTime::now_utc();
        if m.len() > 20_000 {
            m.retain(|_, s| {
                let last = s.warm.max(s.upload);
                s.uploading > 0 || last.is_some_and(|t| now - t < KEEP)
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

    /// An upload of `person` starts: it counts as in progress until the guard is dropped.
    #[must_use]
    pub fn uploading(&self, person: &str) -> Uploading {
        self.with(person, |s| {
            s.uploading += 1;
            s.upload = Some(time::OffsetDateTime::now_utc());
        });
        Uploading(self.clone(), person.to_owned())
    }

    /// `{"generatedAt", "startedAt", "uptimeSeconds", "complete", "persons": {email: {"lastWarm", "lastUpload",
    /// "uploading"}}}`,
    /// entries of the last hour; `complete` is `false` while this replica has been up less than [`LOOKBACK`].
    #[must_use]
    pub fn report(&self) -> Value {
        let now = time::OffsetDateTime::now_utc();
        let m = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut persons = Map::new();
        for (p, s) in m.iter() {
            // an upload in progress is activity now
            let upload = if s.uploading > 0 { Some(now) } else { s.upload };
            let last = s.warm.max(upload);
            if last.is_none_or(|t| now - t >= KEEP) {
                continue;
            }
            persons.insert(
                p.clone(),
                json!({"lastWarm": s.warm.map(Timestamp), "lastUpload": upload.map(Timestamp),
                       "uploading": s.uploading}),
            );
        }
        let up = self.2.elapsed();
        json!({"generatedAt": Timestamp(now), "startedAt": Timestamp(self.1), "uptimeSeconds": up.as_secs(),
               "complete": up >= LOOKBACK, "persons": persons})
    }
}

async fn activity(State(app): State<App>) -> Response {
    crate::http::json(StatusCode::OK, &app.legacy().0.activity.report())
}

/// The ops port's internal routes.
pub fn router(app: App) -> Router {
    Router::new().route(PATH, get(activity)).with_state(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_upload_in_progress_is_activity_now() {
        let a = Activity::default();
        let u = a.uploading("a@example.org");
        let r = a.report();
        let p = &r["persons"]["a@example.org"];
        assert_eq!(p["uploading"], 1);
        assert_eq!(p["lastUpload"], r["generatedAt"], "in progress: now");
        drop(u);
        let r = a.report();
        assert_eq!(r["persons"]["a@example.org"]["uploading"], 0);
        assert!(
            r["persons"]["a@example.org"]["lastUpload"].is_string(),
            "its end stamped"
        );
    }
}

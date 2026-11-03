//! The synchronous ingest of one old asset (DESIGN 9.5, magpie 8.2 step 4): when a send names an old asset path
//! that neither `uploads.legacy_path` nor `import_uploads` knows, magpie asks the import Deployment to ingest that one
//! file from the person's old asset folder now (`POST /v1/ingest-upload`, read-only on the source, at most 5 s), so a
//! file attached a second before the move and sent after it resolves. The import Deployment writes the
//! `import_uploads` row; magpie then looks it up again. Gone in A6 with the import Deployment (the legacy-path
//! resolution itself stays until A7).

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::limits;

/// The import Deployment at its fixed endpoint name (as `http://hermit`, magpie 1).
pub const URL: &str = "http://magpie-import";

/// The route the import Deployment answers.
pub const ROUTE: &str = "/v1/ingest-upload";

/// `POST /v1/ingest-upload` → 200 (the file is in `import_uploads` now) or 404 (no such file in the person's old
/// asset folder).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestRequest {
    /// The person.
    pub person: String,
    /// The file name inside the person's old asset folder (the canonical old path,
    /// `magpie_api::legacy::legacy_asset_name`).
    pub legacy_path: String,
}

/// magpie's client of the import Deployment.
#[derive(Debug, Clone)]
pub struct Ingest {
    http: reqwest::Client,
    url: String,
    token: String,
}

impl Ingest {
    /// A client of `url` (`URL` in service, a test server's address in tests), presenting `token`.
    #[must_use]
    pub fn new(http: reqwest::Client, url: String, token: String) -> Self {
        Self { http, url, token }
    }

    /// Asks for one file: `Ok(true)` when it is ingested, `Ok(false)` when the person has no such file.
    ///
    /// # Errors
    /// 503 `unavailable` when the import Deployment cannot be reached or answers anything else (the send can be
    /// retried; the file may well exist).
    pub async fn ingest(&self, person: &str, name: &str) -> Result<bool> {
        let unavailable = || {
            Error::code(
                magpie_api::errors::ErrorCode::Unavailable,
                "the import cannot be reached",
            )
        };
        let resp = self
            .http
            .post(format!("{}{ROUTE}", self.url))
            .bearer_auth(&self.token)
            .timeout(limits::INGEST_TIMEOUT)
            .json(&IngestRequest {
                person: person.to_owned(),
                legacy_path: name.to_owned(),
            })
            .send()
            .await
            .map_err(|e| {
                tracing::warn!(error = %e, "the synchronous ingest could not reach the import");
                unavailable()
            })?;
        match resp.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            s => {
                tracing::warn!(status = s, "the synchronous ingest failed");
                Err(unavailable())
            }
        }
    }
}

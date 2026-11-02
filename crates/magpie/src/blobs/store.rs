//! The bucket (magpie 5.2): one private S3-compatible bucket per deployment, path-style, any endpoint.
//!
//! ```text
//! blobs/<first 2 hex>/<sha256 hex>        the stored bytes
//! thumbs/<sha256 hex>-512.webp            image thumbnail (long edge 512 px), made on first request
//! display/<sha256 hex>-2000.webp          display copy (long edge <= 2000 px) of a non-upload image
//! ```
//!
//! Every object magpie writes is at most 64 MiB (an upload is at most 50 MiB, a sent file 64 MiB, a staged blob is
//! one extracted string of a line of at most 64 MiB), so one `PUT` carries each; magpie 5.2's multipart path for
//! larger objects has nothing to carry. A `PUT` of an existing key is harmless: the key is the content's hash.
//!
//! The bucket is never called inside a transaction (platform 4.3): callers commit first, or call before.

use std::time::Duration;

use aws_sdk_s3::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::ByteStream;
use axum::body::Bytes;

use crate::limits;

/// Where the bucket is and how to sign in to it (`MAGPIE_S3_*`, magpie 18.2): bindings and credentials, no behaviour.
#[derive(Clone)]
pub struct BucketConfig {
    /// `MAGPIE_S3_ENDPOINT`.
    pub endpoint: String,
    /// `MAGPIE_S3_REGION`.
    pub region: String,
    /// `MAGPIE_S3_BUCKET`.
    pub bucket: String,
    /// `MAGPIE_S3_ACCESS_KEY_ID`.
    pub access_key_id: String,
    /// `MAGPIE_S3_SECRET_ACCESS_KEY`.
    pub secret_access_key: String,
}

impl std::fmt::Debug for BucketConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // never the credentials
        f.debug_struct("BucketConfig")
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("bucket", &self.bucket)
            .finish_non_exhaustive()
    }
}

/// A bucket call that failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// No such object.
    #[error("no such object")]
    NotFound,
    /// The bucket could not be reached or refused (retried by the caller).
    #[error("bucket: {0}")]
    Failed(String),
}

/// The bucket.
#[derive(Debug, Clone)]
pub struct Bucket {
    s3: aws_sdk_s3::Client,
    bucket: String,
}

/// The key of a blob's stored bytes.
#[must_use]
pub fn blob_key(hex: &str) -> String {
    format!("blobs/{}/{hex}", &hex[..2.min(hex.len())])
}

/// The key of a blob's thumbnail.
#[must_use]
pub fn thumb_key(hex: &str) -> String {
    format!("thumbs/{hex}-{}.webp", limits::THUMB_EDGE)
}

/// The key of a blob's display copy.
#[must_use]
pub fn display_key(hex: &str) -> String {
    format!("display/{hex}-{}.webp", limits::DISPLAY_EDGE)
}

fn failed<E: std::error::Error + 'static, R: std::fmt::Debug>(e: &SdkError<E, R>) -> StoreError {
    StoreError::Failed(aws_sdk_s3::error::DisplayErrorContext(e).to_string())
}

fn status<E>(e: &SdkError<E, aws_sdk_s3::config::http::HttpResponse>) -> u16 {
    match e {
        SdkError::ServiceError(s) => s.raw().status().as_u16(),
        SdkError::ResponseError(r) => r.raw().status().as_u16(),
        _ => 0,
    }
}

impl Bucket {
    /// A client for `cfg`, on the workspace's rustls/ring HTTPS client (no request is made).
    #[must_use]
    pub fn new(cfg: &BucketConfig) -> Self {
        use aws_smithy_http_client::tls;
        let http = aws_smithy_http_client::Builder::new()
            .tls_provider(tls::Provider::Rustls(tls::rustls_provider::CryptoMode::Ring))
            .build_https();
        let conf = aws_sdk_s3::config::Builder::new()
            .behavior_version(BehaviorVersion::latest())
            .http_client(http)
            .region(Region::new(cfg.region.clone()))
            .endpoint_url(cfg.endpoint.clone())
            .credentials_provider(Credentials::new(
                cfg.access_key_id.clone(),
                cfg.secret_access_key.clone(),
                None,
                None,
                "magpie",
            ))
            .force_path_style(true)
            .retry_config(aws_sdk_s3::config::retry::RetryConfig::standard().with_max_attempts(2))
            .timeout_config(
                aws_sdk_s3::config::timeout::TimeoutConfig::builder()
                    .connect_timeout(Duration::from_secs(5))
                    .operation_timeout(limits::BUCKET_CALL_TIMEOUT)
                    .build(),
            )
            .build();
        Self {
            s3: aws_sdk_s3::Client::from_conf(conf),
            bucket: cfg.bucket.clone(),
        }
    }

    /// Stores `bytes` at `key`.
    ///
    /// # Errors
    /// [`StoreError::Failed`].
    pub async fn put(&self, key: &str, bytes: Bytes, content_type: Option<&str>) -> Result<(), StoreError> {
        let mut req = self
            .s3
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(bytes));
        if let Some(ct) = content_type {
            req = req.content_type(ct);
        }
        req.send().await.map(|_| ()).map_err(|e| failed(&e))
    }

    /// The object at `key`.
    ///
    /// # Errors
    /// [`StoreError::NotFound`], [`StoreError::Failed`].
    pub async fn get(&self, key: &str) -> Result<Bytes, StoreError> {
        let out = self
            .s3
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| {
                if status(&e) == 404 {
                    StoreError::NotFound
                } else {
                    failed(&e)
                }
            })?;
        let body = out
            .body
            .collect()
            .await
            .map_err(|e| StoreError::Failed(format!("reading the body: {e}")))?;
        Ok(body.into_bytes())
    }

    /// Whether an object is at `key` (`HEAD`).
    ///
    /// # Errors
    /// [`StoreError::Failed`].
    pub async fn exists(&self, key: &str) -> Result<bool, StoreError> {
        match self.s3.head_object().bucket(&self.bucket).key(key).send().await {
            Ok(_) => Ok(true),
            Err(e) if status(&e) == 404 => Ok(false),
            Err(e) => Err(failed(&e)),
        }
    }

    /// Deletes the object at `key` (absent is fine).
    ///
    /// # Errors
    /// [`StoreError::Failed`].
    pub async fn delete(&self, key: &str) -> Result<(), StoreError> {
        match self.s3.delete_object().bucket(&self.bucket).key(key).send().await {
            Ok(_) => Ok(()),
            Err(e) if status(&e) == 404 => Ok(()),
            Err(e) => Err(failed(&e)),
        }
    }

    /// One page of the objects under `prefix`: `(key, last modified)` and the token of the next page.
    ///
    /// # Errors
    /// [`StoreError::Failed`].
    pub async fn list(
        &self,
        prefix: &str,
        token: Option<String>,
    ) -> Result<(Vec<(String, Option<time::OffsetDateTime>)>, Option<String>), StoreError> {
        let out = self
            .s3
            .list_objects_v2()
            .bucket(&self.bucket)
            .prefix(prefix)
            .set_continuation_token(token)
            .send()
            .await
            .map_err(|e| failed(&e))?;
        let items = out
            .contents()
            .iter()
            .filter_map(|o| {
                let key = o.key()?.to_owned();
                let at = o
                    .last_modified()
                    .and_then(|d| time::OffsetDateTime::from_unix_timestamp(d.secs()).ok());
                Some((key, at))
            })
            .collect();
        let next = if out.is_truncated() == Some(true) {
            out.next_continuation_token().map(str::to_owned)
        } else {
            None
        };
        Ok((items, next))
    }

    /// Creates the bucket (tests: each makes its own).
    ///
    /// # Errors
    /// [`StoreError::Failed`].
    pub async fn create(&self) -> Result<(), StoreError> {
        self.s3
            .create_bucket()
            .bucket(&self.bucket)
            .send()
            .await
            .map(|_| ())
            .map_err(|e| failed(&e))
    }
}

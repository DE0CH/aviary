//! The legacy upstream's HTTP side (switch-final 4.1): REST calls to the old router as the person, with the same
//! identity headers as the socket (the newest verified token, the pair token, a view's claims under the old key).

use std::time::Duration;

use axum::body::Bytes;
use reqwest::Method;
use serde_json::Value;

use super::{LU_HEADER, Legacy, OLD_UPLOAD, OLD_VIEW_AS_HEADER};
use crate::identity::Identity;
use crate::paseo::transport::Conn;

/// Who an LU HTTP call is made as: the token holder's newest token and a view's claims signed with the old key.
#[derive(Clone)]
pub struct Who {
    token: String,
    view: Option<String>,
}

impl std::fmt::Debug for Who {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Who")
            .field("view", &self.view.is_some())
            .finish_non_exhaustive()
    }
}

/// An LU HTTP call that has no usable answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallError {
    /// 401/403: the tab gets pigeon's signed-out answer.
    SignedOut,
    /// Another status, with the old route's body (the error the old path would have shown).
    Status(u16, Bytes),
    /// No answer.
    Unreachable,
    /// A 2xx whose body could not be read in time: the old route applied the write, its answer is lost.
    AppliedUnread(u16),
}

impl Legacy {
    /// The caller of an HTTP request (`None` when pigeon holds no token for them).
    #[must_use]
    pub fn who(&self, id: &Identity) -> Option<Who> {
        match id {
            Identity::Person(p) => {
                let token = self
                    .0
                    .tokens
                    .newest(&p.email)
                    .map_or_else(|| p.token.clone(), |(t, _)| t);
                Some(Who { token, view: None })
            }
            Identity::ViewAs(v) => {
                let token = self
                    .0
                    .tokens
                    .newest(&v.admin.email)
                    .map_or_else(|| v.admin.token.clone(), |(t, _)| t);
                let view = self.0.tokens.old_token(&crate::viewas::Claims {
                    a: v.admin.email.clone(),
                    u: v.person.clone(),
                    s: v.session.clone(),
                    e: v.exp,
                });
                Some(Who {
                    token,
                    view: Some(view),
                })
            }
            _ => None,
        }
    }

    /// The caller of a socket's call.
    #[must_use]
    pub fn who_of_conn(&self, conn: &Conn) -> Option<Who> {
        match &conn.viewer {
            None => self
                .0
                .tokens
                .newest(&conn.person)
                .map(|(token, _)| Who { token, view: None }),
            Some(v) => {
                let (token, _) = self.0.tokens.newest(&v.admin)?;
                let view = self.0.tokens.old_token(&crate::viewas::Claims {
                    a: v.admin.clone(),
                    u: conn.person.clone(),
                    s: v.session.clone(),
                    e: v.exp,
                });
                Some(Who {
                    token,
                    view: Some(view),
                })
            }
        }
    }

    /// A request to the old router as `who` (`path` with its query).
    pub fn request(&self, method: Method, path: &str, who: &Who) -> reqwest::RequestBuilder {
        let mut b = self
            .0
            .http
            .request(method, format!("{}{path}", self.0.base))
            .header("cf-access-jwt-assertion", &who.token)
            .header(LU_HEADER, &self.0.pair);
        if let Some(v) = &who.view {
            b = b.header(OLD_VIEW_AS_HEADER, v);
        }
        b
    }

    /// Sends `b` and reads the whole answer, both within `bound`; 2xx is the body, anything else a [`CallError`] (a
    /// 2xx whose body was not read in time is [`CallError::AppliedUnread`]: the write was applied).
    ///
    /// # Errors
    /// The old router's refusal or no answer.
    pub async fn send(&self, b: reqwest::RequestBuilder, bound: Duration) -> Result<(u16, Bytes), CallError> {
        let deadline = tokio::time::Instant::now() + bound;
        let r = match tokio::time::timeout_at(deadline, b.send()).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "LU HTTP failed");
                self.0.metrics.lu_errors.inc();
                return Err(CallError::Unreachable);
            }
            Err(_) => {
                self.0.metrics.lu_errors.inc();
                return Err(CallError::Unreachable);
            }
        };
        let status = r.status().as_u16();
        let Ok(Ok(body)) = tokio::time::timeout_at(deadline, r.bytes()).await else {
            self.0.metrics.lu_errors.inc();
            if (200..300).contains(&status) {
                return Err(CallError::AppliedUnread(status));
            }
            return Err(CallError::Unreachable);
        };
        match status {
            200..=299 => Ok((status, body)),
            401 | 403 => {
                self.0.metrics.lu_401.inc();
                Err(CallError::SignedOut)
            }
            s => {
                if s == 503 {
                    self.0.metrics.lu_503.inc();
                }
                Err(CallError::Status(s, body))
            }
        }
    }

    /// Writes one file into the person's old child through the old upload route; the old child's attachment record
    /// (`path` the old asset path).
    ///
    /// # Errors
    /// The old route's refusal or no answer.
    pub async fn old_upload(
        &self,
        who: &Who,
        name: &str,
        mime: &str,
        bytes: Bytes,
        bound: Duration,
    ) -> Result<Value, CallError> {
        let boundary = format!("lu{}", uuid::Uuid::new_v4().simple());
        let safe = name.replace(['"', '\r', '\n', '\\'], "_");
        let mut body = Vec::with_capacity(bytes.len() + 512);
        body.extend_from_slice(
            format!(
                "--{boundary}\r\ncontent-disposition: form-data; name=\"files\"; filename=\"{safe}\"\r\n\
                 content-type: {mime}\r\n\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(&bytes);
        body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
        let b = self
            .request(Method::POST, OLD_UPLOAD, who)
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(body);
        let (status, answer) = self.send(b, bound).await?;
        // a 2xx is a written file whatever its body says
        let v: Value = serde_json::from_slice(&answer).map_err(|_| CallError::AppliedUnread(status))?;
        v.get("attachments")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .filter(|a| a.get("path").and_then(Value::as_str).is_some())
            .cloned()
            .ok_or(CallError::AppliedUnread(status))
    }
}

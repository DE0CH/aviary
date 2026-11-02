//! The Paseo protocol's pieces the HTTP surface needs: the wire constants ([`compat`]) and the two seams the socket
//! package fills ([`SocketHandler`] for `/paseo-ws`, [`RowProjector`] for the magpie row → Paseo row projection of
//! spec 7.8, which the share view uses too).
//!
//! Until the socket package lands, [`NoSocket`] answers every upgrade with 503 and [`BasicProjector`] projects the
//! row kinds without the tool-call mapper or the localised error sentences.

pub mod compat;
pub mod dispatch;
pub mod handshake;
pub mod mapping;
pub mod messages;
pub mod pushes;
pub mod strings;
pub mod transport;

use std::future::Future;
use std::pin::Pin;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::Response;
use magpie_api::rows::{Row, RowItem, ToolStatus, seq_ranges};
use magpie_api::uploads::BlobVariant;
use serde_json::{Map, Value, json};

use crate::App;
use crate::blobs::Signer;
use crate::identity::Identity;

/// What a projection needs besides the row (spec 7.8: a pure function of the row, the locale, the conversation and
/// the blob key).
#[derive(Debug)]
pub struct ProjectCtx<'a> {
    /// `zh*` locale.
    pub zh: bool,
    /// The image URL signer.
    pub signer: &'a Signer,
}

/// The magpie row → Paseo row projection (spec 7.8).
pub trait RowProjector: Send + Sync {
    /// The Paseo row, or `None` for a row kind that shows nothing.
    fn project(&self, row: &Row, ctx: &ProjectCtx<'_>) -> Option<Value>;
}

/// The `/paseo-ws` upgrade (spec 7), for an identity that is a `Person` or a `ViewAs`.
pub trait SocketHandler: Send + Sync {
    /// Answers the upgrade request.
    fn upgrade(&self, app: App, req: Request, id: Identity) -> Pin<Box<dyn Future<Output = Response> + Send>>;

    /// Spec 11.2 step 3: closes every open socket 1012 within `jitter` (request-free ones first), each after its
    /// in-flight requests answered or `rpc_wait` passed. The default has no sockets to close.
    fn drain(
        &self,
        jitter: std::time::Duration,
        rpc_wait: std::time::Duration,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let _ = (jitter, rpc_wait);
        Box::pin(async {})
    }

    /// The replica's sockets and pollers, for a handler that has them.
    fn hub(&self) -> Option<crate::feeds::Hub> {
        None
    }
}

/// The socket before the socket package: every upgrade gets 503.
#[derive(Debug, Clone, Copy)]
pub struct NoSocket;

impl SocketHandler for NoSocket {
    fn upgrade(&self, _app: App, _req: Request, _id: Identity) -> Pin<Box<dyn Future<Output = Response> + Send>> {
        Box::pin(async { crate::http::text(StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable") })
    }
}

/// The interim projection: every row kind of 7.8 with its Paseo shape, a tool row's `detail` as the mapper's
/// `unknown` kind (input and output as given), model-error and failed-turn text as magpie's `message`.
#[derive(Debug, Clone, Copy)]
pub struct BasicProjector;

fn status_name(s: ToolStatus) -> &'static str {
    match s {
        ToolStatus::Running => "running",
        ToolStatus::Completed => "completed",
        ToolStatus::Failed => "failed",
        ToolStatus::Canceled => "canceled",
    }
}

/// A workspace path pigeon may show (spec 7.8: relative, no `..`, no NUL).
#[must_use]
pub fn safe_path(p: &str) -> bool {
    !p.is_empty() && !p.starts_with('/') && !p.contains('\0') && !p.split('/').any(|s| s == "..")
}

impl RowProjector for BasicProjector {
    fn project(&self, row: &Row, ctx: &ProjectCtx<'_>) -> Option<Value> {
        let item = match &row.item {
            RowItem::User(u) => {
                let mut m = Map::new();
                m.insert("type".into(), json!("user_message"));
                m.insert("text".into(), json!(u.text));
                m.insert("messageId".into(), json!(u.message_id));
                if let Some(c) = &u.client_message_id {
                    m.insert("clientMessageId".into(), json!(c));
                }
                if !u.images.is_empty() {
                    let images: Vec<Value> = u
                        .images
                        .iter()
                        .map(|i| {
                            json!({
                                "url": ctx.signer.url(&i.blob_id, BlobVariant::Display),
                                "mimeType": i.mime_type,
                                "thumbnailUrl": ctx.signer.url(&i.blob_id, BlobVariant::Thumb),
                            })
                        })
                        .collect();
                    m.insert("images".into(), Value::Array(images));
                }
                let files: Vec<Value> = u
                    .files
                    .iter()
                    .filter(|f| safe_path(&f.path))
                    .map(|f| {
                        let mime = if f.mime_type.is_empty() {
                            magpie_api::mime::from_name(&f.name).to_owned()
                        } else {
                            f.mime_type.clone()
                        };
                        json!({"type": "uploaded_file", "id": f.path, "fileName": f.name, "mimeType": mime,
                               "size": f.size, "path": f.path})
                    })
                    .collect();
                if !files.is_empty() {
                    m.insert("attachments".into(), Value::Array(files));
                }
                Value::Object(m)
            }
            RowItem::Assistant(a) => json!({"type": "assistant_message", "text": a.text, "messageId": a.message_id}),
            RowItem::Reasoning(r) => json!({"type": "reasoning", "text": r.text}),
            RowItem::Tool(t) => {
                let output = t.result.as_ref().map(|r| r.text.clone());
                let mut m = Map::new();
                m.insert("type".into(), json!("tool_call"));
                m.insert("callId".into(), json!(t.call_id));
                m.insert("name".into(), json!(t.name));
                m.insert("status".into(), json!(status_name(t.status)));
                m.insert(
                    "error".into(),
                    t.error.as_ref().map_or(Value::Null, |e| json!(e.message)),
                );
                m.insert(
                    "detail".into(),
                    json!({"type": "unknown", "input": t.input, "output": output}),
                );
                if let Some(sent) = &t.sent_files {
                    let files: Vec<Value> = sent
                        .iter()
                        .filter(|f| safe_path(&f.path))
                        .map(|f| {
                            json!({"path": f.path, "name": f.name, "size": f.size, "mediaType": f.mime_type,
                                   "isImage": f.is_image, "sent": t.call_id})
                        })
                        .collect();
                    m.insert("metadata".into(), json!({"sentFiles": files}));
                }
                Value::Object(m)
            }
            RowItem::Error(e) => json!({"type": "error", "message": e.message}),
            RowItem::Unknown => return None,
        };
        let _ = ctx.zh;
        let mut out = Map::new();
        out.insert("provider".into(), json!("claude"));
        out.insert("item".into(), item);
        if let Some(t) = row.turn_id {
            out.insert("turnId".into(), json!(t));
        }
        out.insert("timestamp".into(), json!(row.timestamp));
        out.insert("seqStart".into(), json!(row.seq_start));
        out.insert("seqEnd".into(), json!(row.seq_end));
        out.insert("sourceSeqRanges".into(), json!(seq_ranges(&row.seqs)));
        let collapsed: Vec<&str> = if row.seqs.len() > 1 {
            vec!["tool_lifecycle"]
        } else {
            vec![]
        };
        out.insert("collapsed".into(), json!(collapsed));
        Some(Value::Object(out))
    }
}

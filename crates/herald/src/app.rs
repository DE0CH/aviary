//! herald's API (README "API"): the gateway's caller on behalf of a sandbox, the conversation from its verified
//! `Aviary-Sandbox-Conversation`.

use std::sync::Arc;

use aviary_core::auth::{Caller, Callers};
use aviary_core::http::ApiError;
use aviary_core::http::headers::{AVIARY_SANDBOX_CONVERSATION, SANDBOX_IDENTITY};
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::handle;
use crate::telegram::{Attachment, Error as TgError, Format, Kind, Telegram};

/// Telegram's limits: a channel title 1-128 characters, its description 255, a message 4096.
pub const TITLE_MAX: usize = 128;
pub const ABOUT_MAX: usize = 255;
pub const TEXT_MAX: usize = 4096;
/// The Bot API's upload limits: a document up to 50 MB, a photo up to 10 MB; a caption 1024 characters.
pub const FILE_MAX: u64 = 50 * 1024 * 1024;
pub const PHOTO_MAX: u64 = 10 * 1024 * 1024;
pub const CAPTION_MAX: usize = 1024;
/// A file upload's whole multipart body: the file plus 1 MiB for the fields and the multipart framing.
const UPLOAD_BODY_MAX: u64 = FILE_MAX + 1024 * 1024;
/// The longest file name, bytes.
const NAME_MAX: usize = 255;
/// How long a broken-off upload may take to end at Telegram before herald stops waiting for it.
const BROKEN_OFF_WAIT: std::time::Duration = std::time::Duration::from_secs(10);
/// Request bodies: a message at 4096 characters of up to 4 bytes each, with room for the JSON around it.
const BODY_LIMIT: usize = 64 * 1024;

/// What the API needs.
#[derive(Clone)]
pub struct AppState {
    pub telegram: Arc<dyn Telegram>,
    pub key: handle::Key,
    /// Put before every title (`HERALD_TITLE_PREFIX`), so staff can tell these channels apart.
    pub title_prefix: String,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("title_prefix", &self.title_prefix)
            .finish_non_exhaustive()
    }
}

/// The `/v1` router, every route guarded: the gateway only, which may set the sandbox identity.
pub fn router(state: AppState, callers: &Callers) -> Router {
    let guarded = callers.guard().allow(Caller::Gateway, &SANDBOX_IDENTITY).router([
        ("/v1/channels", post(create)),
        ("/v1/channels/{channel}", get(show)),
        ("/v1/channels/{channel}/messages", post(send)),
        // multipart, streamed to Telegram as it arrives: the JSON body limit does not apply (it reads `Body` itself)
        ("/v1/channels/{channel}/files", post(send_file)),
    ]);
    aviary_core::http::standard(guarded.with_state(state), BODY_LIMIT)
}

fn conversation(headers: &HeaderMap) -> Result<String, ApiError> {
    headers
        .get(AVIARY_SANDBOX_CONVERSATION)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| ApiError::bad_request("no Aviary-Sandbox-Conversation: herald is called through the gateway"))
}

fn from_telegram(e: TgError) -> ApiError {
    match e {
        TgError::FloodWait(secs) => ApiError::rate_limited(secs.max(1)),
        TgError::Refused(why) => ApiError::bad_request(format!("Telegram refused the request: {why}")),
        TgError::NoChannel => ApiError::not_found("the channel no longer exists or the bot was removed from it"),
        TgError::Unavailable(why) => {
            tracing::warn!(error = %why, "telegram call failed");
            ApiError::unavailable("Telegram could not be reached; try again shortly", 10)
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    title: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChannelView {
    channel: String,
    title: String,
    invite_link: Option<String>,
}

async fn create(
    State(s): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<ChannelView>), ApiError> {
    let conv = conversation(&headers)?;
    let name = body.title.trim();
    if name.is_empty() {
        return Err(ApiError::bad_request("title is empty"));
    }
    let title = format!("{}{name}", s.title_prefix);
    if title.chars().count() > TITLE_MAX {
        return Err(ApiError::bad_request(format!(
            "title is too long: at most {} characters",
            TITLE_MAX - s.title_prefix.chars().count()
        )));
    }
    let about = body.description.trim();
    if about.chars().count() > ABOUT_MAX {
        return Err(ApiError::bad_request(format!(
            "description is too long: at most {ABOUT_MAX} characters"
        )));
    }
    let created = s.telegram.create_channel(&title, about).await.map_err(from_telegram)?;
    tracing::info!(conversation = %conv, chat = created.chat_id, "channel created");
    Ok((
        StatusCode::CREATED,
        Json(ChannelView {
            channel: s.key.sign(&conv, created.chat_id),
            title,
            invite_link: Some(created.invite_link),
        }),
    ))
}

fn chat_of(s: &AppState, headers: &HeaderMap, channel: &str) -> Result<(String, i64), ApiError> {
    let conv = conversation(headers)?;
    let chat = s.key.verify(&conv, channel).ok_or_else(|| {
        ApiError::not_found(
            "no such channel in this conversation (a channel belongs to the conversation that created it)",
        )
    })?;
    Ok((conv, chat))
}

async fn show(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(channel): Path<String>,
) -> Result<Json<ChannelView>, ApiError> {
    let (_, chat) = chat_of(&s, &headers, &channel)?;
    let info = s.telegram.channel(chat).await.map_err(from_telegram)?;
    Ok(Json(ChannelView {
        channel,
        title: info.title,
        invite_link: info.invite_link,
    }))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendBody {
    text: String,
    #[serde(default)]
    format: Option<String>,
    #[serde(default)]
    silent: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SentView {
    message_id: i64,
}

async fn send(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(channel): Path<String>,
    Json(body): Json<SendBody>,
) -> Result<(StatusCode, Json<SentView>), ApiError> {
    let (conv, chat) = chat_of(&s, &headers, &channel)?;
    let format = match body.format.as_deref() {
        None | Some("plain") => Format::Plain,
        Some("html") => Format::Html,
        Some(_) => return Err(ApiError::bad_request("format is \"plain\" (the default) or \"html\"")),
    };
    let n = body.text.chars().count();
    if body.text.trim().is_empty() {
        return Err(ApiError::bad_request("text is empty"));
    }
    if n > TEXT_MAX {
        return Err(ApiError::bad_request(format!(
            "text is {n} characters; a message holds at most {TEXT_MAX}: send it as several messages"
        )));
    }
    let message_id = s
        .telegram
        .post(chat, &body.text, format, body.silent)
        .await
        .map_err(from_telegram)?;
    tracing::info!(conversation = %conv, chat, message_id, "message sent");
    Ok((StatusCode::CREATED, Json(SentView { message_id })))
}

/// The file name as shown: no `/`, `\\`, `"` or control characters (each becomes `_`), at most 255 bytes.
#[must_use]
pub fn safe_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.trim().chars() {
        let c = if matches!(c, '/' | '\\' | '"') || c.is_control() {
            '_'
        } else {
            c
        };
        if out.len() + c.len_utf8() > NAME_MAX {
            break;
        }
        out.push(c);
    }
    if out.is_empty() { "file".to_owned() } else { out }
}

fn too_large(kind: Kind) -> ApiError {
    ApiError::too_large(match kind {
        Kind::Document => "the file is larger than Telegram takes: at most 50 MB",
        Kind::Photo => "a photo is at most 10 MB on Telegram: send it with \"as\" \"document\" (up to 50 MB)",
    })
}

fn from_multipart(e: multer::Error, kind: Kind) -> ApiError {
    match e {
        multer::Error::StreamSizeExceeded { .. } => too_large(kind),
        multer::Error::FieldSizeExceeded { field_name, .. } => ApiError::bad_request(format!(
            "{} is too long",
            field_name.unwrap_or_else(|| "a field".to_owned())
        )),
        other => ApiError::bad_request(format!("the multipart body is malformed: {other}")),
    }
}

/// A short text field, already bounded by the multipart constraints.
async fn text(field: multer::Field<'static>, kind: Kind) -> Result<String, ApiError> {
    field.text().await.map_err(|e| from_multipart(e, kind))
}

async fn send_file(
    State(s): State<AppState>,
    headers: HeaderMap,
    Path(channel): Path<String>,
    body: Body,
) -> Result<(StatusCode, Json<SentView>), ApiError> {
    let (conv, chat) = chat_of(&s, &headers, &channel)?;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok());
    if declared.is_some_and(|n| n > UPLOAD_BODY_MAX) {
        return Err(too_large(Kind::Document));
    }
    let boundary = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|ct| multer::parse_boundary(ct).ok())
        .ok_or_else(|| ApiError::bad_request("send the file as multipart/form-data: curl -F file=@report.pdf"))?;
    let limits = multer::SizeLimit::new()
        .whole_stream(UPLOAD_BODY_MAX)
        .for_field("caption", 8 * 1024)
        .for_field("format", 64)
        .for_field("silent", 64)
        .for_field("as", 64);
    let mut parts = multer::Multipart::with_constraints(
        body.into_data_stream(),
        boundary,
        multer::Constraints::new().size_limit(limits),
    );
    let (field, file) = options(&mut parts).await?;
    let kind = file.kind;
    let name = file.name.clone();
    // one chunk in flight: the caller is read only as fast as Telegram takes the bytes, so nothing is held whole
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    let upstream = s.telegram.send_file(chat, file, rx);
    tokio::pin!(upstream);
    let feeding = feed(field, parts, tx, kind);
    tokio::pin!(feeding);
    let fed = tokio::select! {
        fed = &mut feeding => fed,
        // Telegram answered before the body ended: it refused (or failed); the rest of the body is not read
        sent = &mut upstream => return finish(sent, &conv, chat, &name),
    };
    match fed {
        Ok(()) => finish(upstream.await, &conv, chat, &name),
        Err(e) => {
            // the body to Telegram ended in an error, so it posts nothing; let that connection end
            let _ = tokio::time::timeout(BROKEN_OFF_WAIT, upstream).await;
            Err(e)
        }
    }
}

/// The parts before the file (caption, format, silent, as), then the file part itself and what to post it as.
async fn options(parts: &mut multer::Multipart<'static>) -> Result<(multer::Field<'static>, Attachment), ApiError> {
    let (mut caption, mut format, mut silent, mut kind) = (None, Format::Plain, false, Kind::Document);
    let field = loop {
        let f = parts
            .next_field()
            .await
            .map_err(|e| from_multipart(e, kind))?
            .ok_or_else(|| ApiError::bad_request("no file: send it as the part \"file\" (curl -F file=@report.pdf)"))?;
        match f.name() {
            Some("file") if f.file_name().is_some() => break f,
            Some("caption") => {
                let c = text(f, kind).await?;
                let n = c.chars().count();
                if n > CAPTION_MAX {
                    return Err(ApiError::bad_request(format!(
                        "caption is {n} characters; a file's caption holds at most {CAPTION_MAX}: post a message for the rest"
                    )));
                }
                caption = (!c.trim().is_empty()).then_some(c);
            }
            Some("format") => {
                format = match text(f, kind).await?.trim() {
                    "" | "plain" => Format::Plain,
                    "html" => Format::Html,
                    _ => return Err(ApiError::bad_request("format is \"plain\" (the default) or \"html\"")),
                };
            }
            Some("silent") => {
                silent = match text(f, kind).await?.trim() {
                    "" | "false" | "0" => false,
                    "true" | "1" => true,
                    _ => return Err(ApiError::bad_request("silent is \"true\" or \"false\"")),
                };
            }
            Some("as") => {
                kind = match text(f, kind).await?.trim() {
                    "" | "document" => Kind::Document,
                    "photo" => Kind::Photo,
                    _ => return Err(ApiError::bad_request("as is \"document\" (the default) or \"photo\"")),
                };
            }
            other => {
                return Err(ApiError::bad_request(format!(
                    "unknown part {:?}: the parts are caption, format, silent and as, then file (a file part needs a file name)",
                    other.unwrap_or("")
                )));
            }
        }
    };
    let file = Attachment {
        kind,
        name: safe_name(field.file_name().unwrap_or("file")),
        content_type: field
            .content_type()
            .map_or_else(|| "application/octet-stream".to_owned(), ToString::to_string),
        caption,
        format,
        silent,
    };
    Ok((field, file))
}

fn finish(
    sent: Result<i64, TgError>,
    conv: &str,
    chat: i64,
    name: &str,
) -> Result<(StatusCode, Json<SentView>), ApiError> {
    let message_id = sent.map_err(from_telegram)?;
    tracing::info!(conversation = %conv, chat, message_id, file = %name, "file sent");
    Ok((StatusCode::CREATED, Json(SentView { message_id })))
}

/// Passes the file part's bytes on, then checks the body ends with no other part. On any refusal the body to Telegram
/// gets an error, so the upload never completes there.
async fn feed(
    mut field: multer::Field<'static>,
    mut parts: multer::Multipart<'static>,
    tx: tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
    kind: Kind,
) -> Result<(), ApiError> {
    let limit = match kind {
        Kind::Document => FILE_MAX,
        Kind::Photo => PHOTO_MAX,
    };
    let sender = &tx;
    let fed = async move {
        let mut n: u64 = 0;
        while let Some(chunk) = field.chunk().await.map_err(|e| from_multipart(e, kind))? {
            n += chunk.len() as u64;
            if n > limit {
                return Err(too_large(kind));
            }
            if sender.send(Ok(chunk)).await.is_err() {
                // Telegram's side ended first: its own answer says why
                return Ok(());
            }
        }
        if n == 0 {
            return Err(ApiError::bad_request("the file is empty"));
        }
        // multer reads the next part only once this one is gone
        drop(field);
        match parts.next_field().await {
            Ok(None) => Ok(()),
            Ok(Some(_)) => Err(ApiError::bad_request(
                "a part after the file: put caption, format, silent and as before the file, and send one file per request",
            )),
            Err(e) => Err(from_multipart(e, kind)),
        }
    }
    .await;
    if fed.is_err() {
        let _ = tx.send(Err(std::io::Error::other("the upload was broken off"))).await;
    }
    fed
}

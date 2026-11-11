//! herald's API (README "API"): the gateway's caller on behalf of a sandbox, the conversation from its verified
//! `Aviary-Sandbox-Conversation`.

use std::sync::Arc;

use aviary_core::auth::{Caller, Callers};
use aviary_core::http::ApiError;
use aviary_core::http::headers::{AVIARY_SANDBOX_CONVERSATION, SANDBOX_IDENTITY};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::handle;
use crate::telegram::{Error as TgError, Format, Telegram};

/// Telegram's limits: a channel title 1-128 characters, its description 255, a message 4096.
pub const TITLE_MAX: usize = 128;
pub const ABOUT_MAX: usize = 255;
pub const TEXT_MAX: usize = 4096;
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

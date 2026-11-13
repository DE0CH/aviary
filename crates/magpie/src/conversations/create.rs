//! Create (M3) and creation lookup (M4), magpie 9.2.
//!
//! `(person, idempotencyKey)` is unique: a repeat from another socket, a reconnect or another pigeon replica answers
//! the same conversation and turn (200, same body). Two concurrent creates with one key: the second insert waits
//! for the first transaction (`on conflict do nothing`) and then reads its conversation.

use aviary_pg::{Pool, Tx};
use magpie_api::Admitted;
use magpie_api::Timestamp;
use magpie_api::conversations::{CreateRequest, CreateResponse, Origin, OriginKind};
use magpie_api::errors::ErrorCode;
use magpie_api::events::{ConversationCreated, EventBody};
use magpie_api::turns::TurnStarted;
use time::OffsetDateTime;
use uuid::Uuid;

use super::{Person, ensure_person, load_owned};
use crate::changes;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::events::{self, NewEvent};
use crate::limits;
use crate::turns;

/// The provisional title of a first message: its first 24 characters, plus `…` when longer; none when empty.
#[must_use]
pub fn provisional_title(text: &str) -> Option<String> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let mut chars = t.chars();
    let head: String = chars.by_ref().take(limits::PROVISIONAL_TITLE_CHARS).collect();
    if chars.next().is_some() {
        Some(format!("{head}…"))
    } else {
        Some(head)
    }
}

/// M3's checks that need no database.
///
/// # Errors
/// 400 `bad_request` / `too_many_attachments`, 413 `too_large`.
pub fn validate(req: &CreateRequest) -> Result<()> {
    let key = req.idempotency_key.as_str();
    if key.trim().is_empty() || key.len() > limits::IDEMPOTENCY_KEY_MAX {
        return Err(Error::bad_request("idempotencyKey must be 1-200 bytes"));
    }
    let m = &req.message;
    if m.text.len() > limits::TURN_TEXT_MAX_BYTES {
        return Err(Error::code(ErrorCode::TooLarge, "the message is over 1 MiB"));
    }
    if m.images.len() > limits::ATTACHMENTS_MAX || m.files.len() > limits::ATTACHMENTS_MAX {
        return Err(Error::code(
            ErrorCode::TooManyAttachments,
            "at most 10 images and 10 files per message",
        ));
    }
    if turns::has_content(m) && m.client_message_id.trim().is_empty() {
        return Err(Error::bad_request("message.clientMessageId is required"));
    }
    Ok(())
}

/// The first turn of a created conversation: the person turn admitted with it (the earliest), if any.
async fn first_turn(db: &impl Db, conversation: Uuid) -> Result<Option<TurnStarted>> {
    let row = db
        .query_opt(
            "select id, created_at from turns where conversation_id = $1 and origin = 'person'
             order by created_at, id limit 1",
            &[&conversation],
        )
        .await?;
    Ok(row.map(|r| TurnStarted {
        id: r.get(0),
        started_at: Timestamp(r.get::<_, OffsetDateTime>(1)),
    }))
}

/// M3.
///
/// # Errors
/// 400 `bad_request` / `too_many_attachments`, 413 `too_large`; the admission's refusals; a statement error.
pub async fn create(pool: &Pool, person: &Person, req: CreateRequest) -> Result<Admitted<CreateResponse>> {
    let model = req.model.clone();
    create_with_model(pool, person, req, model).await
}

/// M3 for a conversation the deployment starts (hermit's route): `model` is its `pickedModel` from the start.
///
/// # Errors
/// As [`create`]; 400 `bad_request` for a model id that is empty or over 200 characters.
pub async fn create_with_model(
    pool: &Pool,
    person: &Person,
    req: CreateRequest,
    model: Option<String>,
) -> Result<Admitted<CreateResponse>> {
    validate(&req)?;
    let model = model.map(|m| m.trim().to_owned());
    if model
        .as_deref()
        .is_some_and(|m| m.is_empty() || m.chars().count() > magpie_api::conversations::MODEL_MAX_CHARS)
    {
        return Err(Error::bad_request("model must be 1-200 characters"));
    }
    if req
        .thinking
        .as_deref()
        .is_some_and(|l| l.trim().is_empty() || l.chars().count() > magpie_api::conversations::MODEL_MAX_CHARS)
    {
        return Err(Error::bad_request("thinking must be 1-200 characters"));
    }
    let (req, model) = (&req, &model); // borrowed: the transaction may run more than once (aviary_pg::tx!)
    let (new, id) = aviary_pg::tx!(pool, |t| -> Result<(bool, Uuid)> {
        create_tx(t, person, req, model.as_deref()).await
    })
    .await?;
    let conn = pool.get().await?;
    let conversation = load_owned(&conn, &person.email, id).await?;
    let turn = first_turn(&conn, id).await?;
    let body = CreateResponse { conversation, turn };
    Ok(if new {
        Admitted::New(body)
    } else {
        Admitted::Repeat(body)
    })
}

async fn create_tx(t: &Tx<'_>, person: &Person, req: &CreateRequest, model: Option<&str>) -> Result<(bool, Uuid)> {
    ensure_person(t, &person.email).await?;
    let id = Uuid::new_v4();
    let now = OffsetDateTime::now_utc();
    let title = provisional_title(&req.message.text);
    let origin = Origin {
        kind: OriginKind::New,
        share_id: None,
    };
    let inserted = t
        .query_opt(
            "insert into conversations (id, person, create_key, title, created_at, activity_at, origin, picked_model,
                                        picked_thinking, picked_harness)
             values ($1, $2, $3, $4, $5, $5, $6, $7, $8, $9)
             on conflict (person, create_key) do nothing
             returning id",
            &[
                &id,
                &person.email,
                &req.idempotency_key,
                &title,
                &now,
                &serde_json::to_value(&origin)?,
                &model,
                &req.thinking.as_deref().map(str::trim),
                &req.harness.as_str(),
            ],
        )
        .await?;
    if inserted.is_none() {
        let row = t
            .query_one(
                "select id from conversations where person = $1 and create_key = $2",
                &[&person.email, &req.idempotency_key],
            )
            .await?;
        return Ok((false, row.get(0)));
    }
    // the row is ours and new: lock it for the writers below (one lock order, magpie 4)
    t.execute("select 1 from conversations where id = $1 for update", &[&id])
        .await?;
    let created = EventBody::ConversationCreated(ConversationCreated {
        title: title.clone(),
        origin,
    });
    events::append(t, id, NewEvent::of(&created)?.viewer(person.viewer_email())).await?;
    if let Some(v) = &person.viewer {
        t.execute(
            "insert into view_as_log (admin, person, session, event, conversation_id) values ($1, $2, $3, 'create', $4)",
            &[&v.email, &person.email, &v.session, &id],
        )
        .await?;
    }
    if turns::has_content(&req.message) {
        turns::admit_first(t, id, person, &req.message).await?;
    }
    changes::upserted(t, &person.email, id).await?;
    Ok((true, id))
}

/// Whether the person already made the create with `key` (a repeat admits nothing).
///
/// # Errors
/// When the statement fails.
pub async fn exists(pool: &Pool, person: &Person, key: &str) -> Result<bool> {
    Ok(pool
        .get()
        .await?
        .query_opt(
            "select 1 from conversations where person = $1 and create_key = $2",
            &[&person.email, &key],
        )
        .await?
        .is_some())
}

/// M4: the person's create with `key`.
///
/// # Errors
/// 404 `not_found` when there is none; a statement error.
pub async fn creation(pool: &Pool, person: &Person, key: &str) -> Result<CreateResponse> {
    let conn = pool.get().await?;
    let row = conn
        .query_opt(
            "select id from conversations where person = $1 and create_key = $2",
            &[&person.email, &key],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let id: Uuid = row.get(0);
    let conversation = load_owned(&conn, &person.email, id).await?;
    let turn = first_turn(&conn, id).await?;
    Ok(CreateResponse { conversation, turn })
}

#[cfg(test)]
mod tests {
    use super::provisional_title;

    #[test]
    fn the_provisional_title_is_the_first_24_characters() {
        assert_eq!(provisional_title(""), None);
        assert_eq!(provisional_title("  \n "), None);
        assert_eq!(provisional_title("短"), Some("短".to_owned()));
        let exact: String = "港".repeat(24);
        assert_eq!(provisional_title(&exact), Some(exact.clone()));
        let long = "幫我整理今日有關港珠澳大橋嘅新聞，越詳細越好，唔該晒";
        let t = provisional_title(long).unwrap();
        assert_eq!(t.chars().count(), 25);
        assert!(t.ends_with('…'));
        assert!(long.starts_with(t.trim_end_matches('…')));
        // scalar values, not bytes: an emoji is one
        let emoji = "😀".repeat(30);
        assert_eq!(provisional_title(&emoji).unwrap().chars().count(), 25);
    }
}

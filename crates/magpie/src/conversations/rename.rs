//! M5 `PATCH /v1/conversations/{id}`: a rename (magpie 9.4-9.5: it always wins over a generated or provisional
//! title), a model pick (the conversation's turns run on it from the next one), or both.

use aviary_pg::Pool;
use magpie_api::conversations::{Conversation, MODEL_MAX_CHARS, RenameRequest};
use magpie_api::errors::ErrorCode;
use magpie_api::events::{EventBody, NameSource, TitleChanged};
use uuid::Uuid;

use super::{Person, load_owned, lock};
use crate::changes;
use crate::error::{Error, Result};
use crate::events::{self, NewEvent};
use crate::limits;

/// M5.
///
/// # Errors
/// 400 `empty_title` / `title_too_long` / `bad_request` (neither a title nor a model, or a model id that is empty
/// or over 200 characters), 403 `view_as_read_only`, 404; a statement error.
pub async fn rename(pool: &Pool, person: &Person, id: Uuid, req: RenameRequest) -> Result<Conversation> {
    person.refuse_view_as()?;
    let title = match req.title {
        None => None,
        Some(t) => {
            let t = t.trim().to_owned();
            if t.is_empty() {
                return Err(Error::code(ErrorCode::EmptyTitle, "the title is empty"));
            }
            if t.chars().count() > limits::RENAME_MAX_CHARS {
                return Err(Error::code(ErrorCode::TitleTooLong, "the title is over 200 characters"));
            }
            Some(t)
        }
    };
    let model = match req.model {
        None => None,
        Some(m) => {
            let m = m.trim().to_owned();
            if m.is_empty() || m.chars().count() > MODEL_MAX_CHARS {
                return Err(Error::bad_request("model must be 1-200 characters"));
            }
            Some(m)
        }
    };
    // `""`: no level picked again
    let thinking = match req.thinking {
        None => None,
        Some(l) => {
            let l = l.trim().to_owned();
            if l.chars().count() > MODEL_MAX_CHARS {
                return Err(Error::bad_request("thinking must be at most 200 characters"));
            }
            Some((!l.is_empty()).then_some(l))
        }
    };
    if title.is_none() && model.is_none() && thinking.is_none() {
        return Err(Error::bad_request("a title, a model or a thinking level is required"));
    }
    // borrowed: the transaction may run more than once (aviary_pg::tx!)
    let (title, model, thinking) = (&title, &model, &thinking);
    let harness = req.harness.unwrap_or_default();
    aviary_pg::tx!(pool, |t| -> Result<()> {
        lock(t, &person.email, id).await?;
        if let Some(title) = title {
            t.execute(
                "update conversations set title = $2, name_source = 'user' where id = $1",
                &[&id, title],
            )
            .await?;
            let body = EventBody::TitleChanged(TitleChanged {
                title: title.clone(),
                name_source: Some(NameSource::User),
            });
            events::append(t, id, NewEvent::of(&body)?).await?;
        }
        if let Some(model) = model {
            // the pick names the model's harness with it (absent: Claude Code)
            t.execute(
                "update conversations set picked_model = $2, picked_harness = $3 where id = $1",
                &[&id, model, &harness.as_str()],
            )
            .await?;
        }
        if let Some(level) = thinking {
            t.execute(
                "update conversations set picked_thinking = $2 where id = $1",
                &[&id, level],
            )
            .await?;
        }
        changes::upserted(t, &person.email, id).await?;
        Ok(())
    })
    .await?;
    let conn = pool.get().await?;
    load_owned(&conn, &person.email, id).await
}

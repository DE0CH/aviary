//! Rename (M5), magpie 9.4-9.5: a rename always wins over a generated or provisional title.

use aviary_pg::Pool;
use magpie_api::conversations::{Conversation, RenameRequest};
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
/// 400 `empty_title` / `title_too_long`, 403 `view_as_read_only`, 404; a statement error.
pub async fn rename(pool: &Pool, person: &Person, id: Uuid, req: RenameRequest) -> Result<Conversation> {
    person.refuse_view_as()?;
    let title = req.title.trim().to_owned();
    if title.is_empty() {
        return Err(Error::code(ErrorCode::EmptyTitle, "the title is empty"));
    }
    if title.chars().count() > limits::RENAME_MAX_CHARS {
        return Err(Error::code(ErrorCode::TitleTooLong, "the title is over 200 characters"));
    }
    aviary_pg::tx!(pool, |t| -> Result<()> {
        lock(t, &person.email, id).await?;
        t.execute(
            "update conversations set title = $2, name_source = 'user' where id = $1",
            &[&id, &title],
        )
        .await?;
        let body = EventBody::TitleChanged(TitleChanged {
            title: title.clone(),
            name_source: Some(NameSource::User),
        });
        events::append(t, id, NewEvent::of(&body)?).await?;
        changes::upserted(t, &person.email, id).await?;
        Ok(())
    })
    .await?;
    let conn = pool.get().await?;
    load_owned(&conn, &person.email, id).await
}

//! Conversations from sessions rows (cutover 7.4.1, magpie 19.1): a new row creates the conversation with the same
//! id, `migrating = true`, origin `import`, its `conversation.created` event and its `import_sources` row; a changed
//! row makes the changes a person's action would have made (title with `name_source`, Recently deleted, restore,
//! model); a row that vanished while migrating purges the conversation. Every write locks the conversation row and
//! re-checks `migrating` first (cutover 6.2 items 1-2).

use magpie::changes;
use magpie::conversations::{delete, ensure_person};
use magpie::events::{self, NewEvent};
use magpie_api::conversations::{Origin, OriginKind};
use magpie_api::events::{ConversationCreated, EventBody, NameSource, TitleChanged};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::source::{SessionRow, parse_time};
use crate::sync::Files;
use crate::{Error, Importer, Result};

/// What [`create`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Created {
    /// Created now.
    New,
    /// Imported before (its `import_sources` row exists).
    Imported,
    /// A conversation with this id exists and was not imported: an id conflict (a `plan` blocker).
    Conflict,
    /// The conversation was deleted for good on the new side (its `import_purged` tombstone): it is not created
    /// again from the old side's row, which the reverse export deletes.
    Purged,
}

/// The old title's `name_source` in magpie's terms (`title`, `user`; anything else is provisional).
#[must_use]
pub fn name_source(s: Option<&str>) -> Option<NameSource> {
    match s {
        Some("title") => Some(NameSource::Title),
        Some("user") => Some(NameSource::User),
        _ => None,
    }
}

fn ns_str(n: Option<NameSource>) -> Option<&'static str> {
    n.map(|n| match n {
        NameSource::Title => "title",
        NameSource::User => "user",
    })
}

fn title_of(s: &SessionRow) -> Option<String> {
    s.custom_name
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| t.chars().take(magpie::limits::RENAME_MAX_CHARS).collect())
}

/// Creates the migrating conversation of sessions row `s` of `person` (schema `schema`, key `key`). `first_record`
/// is the transcript's first record time, used for the 1970 placeholder.
///
/// # Errors
/// [`Error::Source`] for a row whose id is not a uuid; a statement error.
pub async fn create(
    imp: &Importer,
    person: &str,
    schema: &str,
    s: &SessionRow,
    first_record: Option<OffsetDateTime>,
) -> Result<Created> {
    let id = s.id()?;
    let created = s
        .created_at
        .as_deref()
        .and_then(parse_time)
        .or(first_record)
        .unwrap_or_else(OffsetDateTime::now_utc);
    let activity = s.updated_at.as_deref().and_then(parse_time).unwrap_or(created);
    let title = title_of(s);
    let ns = ns_str(name_source(s.name_source.as_deref()));
    let psid = s.psid().to_owned();
    let main = crate::tree::main_path(&psid);
    let key = schema.strip_prefix("cc_").unwrap_or(schema).to_owned();
    let fp = s.fingerprint();
    let deleted = s.deleted();
    let model = s.model.clone().filter(|m| !m.is_empty());
    let pool = &imp.magpie.pool;
    aviary_pg::tx!(pool, |t| -> Result<Created> {
        imp.check_lock_in(t).await?;
        if let Some(r) = t
            .query_opt(
                "select exists (select 1 from import_sources where conversation_id = $1) from conversations
                 where id = $1",
                &[&id],
            )
            .await?
        {
            return Ok(if r.get::<_, bool>(0) {
                Created::Imported
            } else {
                Created::Conflict
            });
        }
        // a purge on the new side cascades to `import_sources`, so only its tombstone tells the importer that this
        // sessions row was imported, released and then deleted for good there (checked in the create's transaction:
        // the purge writes the tombstone in its own)
        if t.query_opt("select 1 from import_purged where conversation_id = $1", &[&id])
            .await?
            .is_some()
        {
            return Ok(Created::Purged);
        }
        ensure_person(t, person).await?;
        let origin = Origin {
            kind: OriginKind::Import,
            share_id: None,
        };
        t.execute(
            "insert into conversations (id, person, title, name_source, created_at, activity_at, origin, migrating,
                                        provider_session_id, model, deleted_at)
             values ($1, $2, $3, $4, $5, $6, $7, true, $8, $9, $10)",
            &[
                &id,
                &person,
                &title,
                &ns,
                &created,
                &activity,
                &serde_json::to_value(&origin)?,
                &psid,
                &model,
                &deleted,
            ],
        )
        .await?;
        let body = EventBody::ConversationCreated(ConversationCreated { title, origin });
        let mut ev = NewEvent::of(&body)?;
        ev.at = created;
        events::append(t, id, ev).await?;
        magpie::ccfs::lines::set_main_transcript(t, id, &main).await?;
        t.execute(
            "insert into import_sources (conversation_id, person, user_key, source_schema, provider_session, row_fp,
                                         files, sent_fp, first_imported_at, last_synced_at, last_change_at)
             values ($1, $2, $3, $4, $5, $6, '{}', '', now(), now(), now())",
            &[&id, &person, &key, &schema, &psid, &fp],
        )
        .await?;
        changes::upserted(t, person, id).await?;
        Ok(Created::New)
    })
    .await
}

/// Applies sessions row `s` to its migrating conversation: the title (with `name_source`, a `title.changed` event
/// when it changed), Recently deleted or restored, the model, the activity time; records the row's fingerprint.
/// Whether anything changed.
///
/// # Errors
/// [`Error::Released`] when the conversation is no longer migrating; a statement error.
pub async fn apply_row(imp: &Importer, person: &str, s: &SessionRow) -> Result<bool> {
    let id = s.id()?;
    let title = title_of(s);
    let ns = name_source(s.name_source.as_deref());
    let deleted = s.deleted();
    let model = s.model.clone().filter(|m| !m.is_empty());
    let activity = s.updated_at.as_deref().and_then(parse_time);
    let fp = s.fingerprint();
    let pool = &imp.magpie.pool;
    aviary_pg::tx!(pool, |t| -> Result<bool> {
        imp.check_lock_in(t).await?;
        let Some(r) = t
            .query_opt(
                "select migrating, title, name_source, deleted_at, model, person, provider_session_id from conversations
                 where id = $1 for update",
                &[&id],
            )
            .await?
        else {
            return Err(Error::Released);
        };
        if r.get::<_, String>(5) != person {
            return Err(another_persons(id));
        }
        if !r.get::<_, bool>(0) {
            return Err(Error::Released);
        }
        let mut changed = false;
        let old_title: Option<String> = r.get(1);
        let old_ns: Option<String> = r.get(2);
        let old_deleted: Option<OffsetDateTime> = r.get(3);
        let old_model: Option<String> = r.get(4);
        if let Some(title) = &title
            && (old_title.as_ref() != Some(title) || old_ns.as_deref() != ns_str(ns))
        {
            t.execute(
                "update conversations set title = $2, name_source = $3 where id = $1",
                &[&id, &title, &ns_str(ns)],
            )
            .await?;
            let body = EventBody::TitleChanged(TitleChanged {
                title: title.clone(),
                name_source: ns,
            });
            events::append(t, id, NewEvent::of(&body)?).await?;
            changed = true;
        } else if title.is_none()
            && old_ns.is_some()
            && let Some(old) = &old_title
        {
            // the title was cleared on the old side: the conversation keeps its words, as a provisional title the
            // title model may replace (magpie has no untitled-again action)
            t.execute("update conversations set name_source = null where id = $1", &[&id])
                .await?;
            let body = EventBody::TitleChanged(TitleChanged {
                title: old.clone(),
                name_source: None,
            });
            events::append(t, id, NewEvent::of(&body)?).await?;
            changed = true;
        }
        if deleted.map(OffsetDateTime::unix_timestamp) != old_deleted.map(OffsetDateTime::unix_timestamp) {
            t.execute(
                "update conversations set deleted_at = $2 where id = $1",
                &[&id, &deleted],
            )
            .await?;
            changed = true;
        }
        if model.is_some() && model != old_model {
            t.execute("update conversations set model = $2 where id = $1", &[&id, &model])
                .await?;
            changed = true;
        }
        changed |= follow_session(t, id, s.psid(), r.get::<_, Option<String>>(6).as_deref()).await?;
        if let Some(a) = activity {
            t.execute(
                "update conversations set activity_at = greatest(activity_at, $2) where id = $1",
                &[&id, &a],
            )
            .await?;
        }
        t.execute(
            "update import_sources set row_fp = $2, last_synced_at = now() where conversation_id = $1",
            &[&id, &fp],
        )
        .await?;
        if changed {
            changes::upserted(t, person, id).await?;
        }
        Ok(changed)
    })
    .await
}

/// Claude Code's session id arrived (or changed) after the import: a conversation imported while its first turn ran
/// has no `provider_session_id` on its row yet, so it was created under its own id. The transcript the pass now syncs
/// is the session id's; the conversation, its record and its main transcript follow it, else verification looks for
/// a transcript that is not there and the conversation is never released. Whether it changed.
async fn follow_session(t: &aviary_pg::Tx<'_>, id: Uuid, psid: &str, recorded: Option<&str>) -> Result<bool> {
    if recorded == Some(psid) {
        return Ok(false);
    }
    t.execute(
        "update conversations set provider_session_id = $2 where id = $1",
        &[&id, &psid],
    )
    .await?;
    t.execute(
        "update import_sources set provider_session = $2 where conversation_id = $1",
        &[&id, &psid],
    )
    .await?;
    magpie::ccfs::lines::set_main_transcript(t, id, &crate::tree::main_path(psid)).await?;
    Ok(true)
}

/// The error of a write for one person to another person's conversation (a sessions row id in two schemas, or a
/// person whose schema was renamed): reported, never taken for a release.
#[must_use]
pub fn another_persons(id: Uuid) -> Error {
    Error::Source(format!("{id} is another person's conversation in magpie: not written"))
}

/// Records a sync's results on the `import_sources` row (only while the conversation is migrating).
///
/// # Errors
/// [`Error::Released`] when it is not; a statement error.
pub async fn record(
    imp: &Importer,
    id: Uuid,
    files: &Files,
    failed_fp: &str,
    sent_fp: &str,
    changed: bool,
) -> Result<()> {
    let files = serde_json::to_value(files)?;
    let pool = &imp.magpie.pool;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        imp.check_lock_in(t).await?;
        let n = t
            .execute(
                "update import_sources s set files = $2, failed_turns_fp = $3, sent_fp = $5, last_synced_at = now(),
                        last_change_at = case when $4 then now() else last_change_at end, last_error = null
                 from conversations c
                 where s.conversation_id = $1 and c.id = s.conversation_id and c.migrating",
                &[&id, &files, &failed_fp, &changed, &sent_fp],
            )
            .await?;
        if n == 0 {
            return Err(Error::Released);
        }
        Ok(())
    })
    .await
}

/// Ends a sync that wrote cc files: the `truncated` limbo its operations left (lines a rewrite removed, credited to
/// no turn) becomes one `retract` now, as a turn's end does for a live sandbox (magpie 6.7), so the conversation's
/// visible events equal the classification of the source's current lines after every pass. How many lines.
///
/// # Errors
/// [`Error::Released`] when the conversation is not migrating; a statement error.
pub async fn flush_limbo(imp: &Importer, id: Uuid) -> Result<usize> {
    let pool = &imp.magpie.pool;
    aviary_pg::tx!(pool, |t| -> Result<usize> {
        imp.check_lock_in(t).await?;
        let Some(r) = t
            .query_opt("select migrating from conversations where id = $1 for update", &[&id])
            .await?
        else {
            return Err(Error::Released);
        };
        if !r.get::<_, bool>(0) {
            return Err(Error::Released);
        }
        Ok(magpie::ccfs::limbo::flush_scope(t, id, None).await?)
    })
    .await
}

/// Records a sync error (the pass goes on, cutover 7.5).
///
/// # Errors
/// A statement error.
pub async fn record_error(imp: &Importer, id: Uuid, error: &str) -> Result<()> {
    imp.magpie
        .pool
        .get()
        .await?
        .execute(
            "update import_sources set last_error = $2 where conversation_id = $1",
            &[&id, &error],
        )
        .await?;
    Ok(())
}

/// A sessions row that vanished while its conversation is migrating (`CloudCLI`'s purge, a permanent delete):
/// magpie's purge of its rows ([`delete::forget_locked`]: no hermit call, the workspace is the old stack's),
/// `import_sources` with it. Nothing after the release. Whether it was purged.
///
/// # Errors
/// A statement error.
pub async fn purge(imp: &Importer, id: Uuid) -> Result<bool> {
    let pool = &imp.magpie.pool;
    aviary_pg::tx!(pool, |t| -> Result<bool> {
        imp.check_lock_in(t).await?;
        let Some(r) = t
            .query_opt(
                "select person, migrating from conversations where id = $1 for update",
                &[&id],
            )
            .await?
        else {
            return Ok(false);
        };
        if !r.get::<_, bool>(1) {
            return Ok(false);
        }
        let person: String = r.get(0);
        delete::forget_locked(t, &person, id).await?;
        Ok(true)
    })
    .await
}

/// The conversation's id for a sessions row id, when it parses.
#[must_use]
pub fn id_of(session_id: &str) -> Option<Uuid> {
    Uuid::parse_str(session_id).ok()
}

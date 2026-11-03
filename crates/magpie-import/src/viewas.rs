//! The view-as audit (cutover 7.4.5): `jarvis.ops_events` rows of kind `view_as` become magpie `view_as_log` rows.
//! New rows can appear until the old ui is gone (A5), so every pass copies the ones not there yet; a row is the
//! same row when its time, admin, person, session and event are (the log has no source id), so a repeat copies
//! nothing. The old `conversation` event (a send while viewing as) is magpie's `send`; an event magpie's log has no
//! name for is skipped and counted.

use aviary_pg::Pool;
use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::{Importer, Result};

/// magpie's event for an old one.
#[must_use]
pub fn event_of(old: &str) -> Option<&'static str> {
    Some(match old {
        "start" => "start",
        "conversation" | "send" => "send",
        "create" => "create",
        "cancel" => "cancel",
        "upload" => "upload",
        "admin_read" => "admin_read",
        _ => return None,
    })
}

/// What a view-as pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Copied {
    /// Rows written now.
    pub copied: u64,
    /// Rows already there.
    pub present: u64,
    /// Rows with an event magpie's log has no name for.
    pub skipped: u64,
}

/// Copies the view-as rows of `jarvis` not in magpie yet.
///
/// # Errors
/// A statement error.
pub async fn pass(imp: &Importer, jarvis: &Pool) -> Result<Copied> {
    let rows = aviary_pg::tx!(jarvis, |t| -> Result<Vec<aviary_pg::Row>> {
        t.batch_execute("set transaction isolation level repeatable read, read only")
            .await?;
        Ok(t.query(
            "select at, detail->>'admin', detail->>'user', coalesce(detail->>'session', ''), detail->>'event',
                    conversation_id, (detail->>'seq')::bigint, (detail->>'chars')::int4
             from ops_events where kind = 'view_as' order by id",
            &[],
        )
        .await?)
    })
    .await?;
    let mut out = Copied::default();
    let conn = imp.magpie.pool.get().await?;
    for r in rows {
        let at: OffsetDateTime = r.get(0);
        let (Some(admin), Some(person)) = (r.get::<_, Option<String>>(1), r.get::<_, Option<String>>(2)) else {
            out.skipped += 1;
            continue;
        };
        let session: String = r.get(3);
        let Some(event) = r.get::<_, Option<String>>(4).as_deref().and_then(event_of) else {
            out.skipped += 1;
            continue;
        };
        let conversation: Option<Uuid> = r.get(5);
        let seq: Option<i64> = r.get(6);
        let chars: Option<i32> = r.get(7);
        let admin = aviary_core::log::normalise_email(&admin);
        let person = aviary_core::log::normalise_email(&person);
        let n = conn
            .execute(
                "insert into view_as_log (at, admin, person, session, event, conversation_id, seq, chars)
                 select $1, $2, $3, $4, $5, $6, $7, $8
                 where not exists (select 1 from view_as_log where at = $1 and admin = $2 and person = $3
                                     and session = $4 and event = $5)",
                &[&at, &admin, &person, &session, &event, &conversation, &seq, &chars],
            )
            .await?;
        if n == 0 {
            out.present += 1;
        } else {
            out.copied += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn old_events_map_to_magpies() {
        assert_eq!(super::event_of("conversation"), Some("send"));
        assert_eq!(super::event_of("start"), Some("start"));
        assert_eq!(super::event_of("something"), None);
    }
}

//! The old stack's database `cloudcli` (cutover 7.2, 7.4 step 1): one `repeatable read`, read-only transaction over
//! every `cc_u*` schema (`users`, `sessions`, `jarvis_failed_turns`), and the text timestamps of magpie 19.1.

use std::collections::BTreeMap;

use aviary_pg::Pool;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

use crate::{Error, Result};

/// A `CloudCLI` sessions row (`schema.ts`, version 1 and 3), as text the way `CloudCLI` stores it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRow {
    /// `session_id`: the conversation id.
    pub session_id: String,
    /// `provider` (`claude`).
    pub provider: String,
    /// Claude Code's session id (the transcript's file name).
    pub provider_session_id: Option<String>,
    /// The title.
    pub custom_name: Option<String>,
    /// `null` (provisional), `title` or `user`.
    pub name_source: Option<String>,
    /// `"isArchived"`: 1 with `deleted_at` = in Recently deleted.
    pub is_archived: i32,
    /// When it went to Recently deleted (text).
    pub deleted_at: Option<String>,
    /// The transcript's path in the old pod.
    pub jsonl_path: Option<String>,
    /// Text timestamps (either format; `1970-…` is a placeholder).
    pub created_at: Option<String>,
    /// Text timestamp.
    pub updated_at: Option<String>,
    /// The model `CloudCLI` last ran it with.
    pub model: Option<String>,
}

impl SessionRow {
    /// The conversation id (`session_id` is a uuid for every row `CloudCLI` or its synchronizer writes).
    ///
    /// # Errors
    /// [`Error::Source`] when it is not one.
    pub fn id(&self) -> Result<Uuid> {
        Uuid::parse_str(&self.session_id)
            .map_err(|_| Error::Source(format!("sessions row {} is not a uuid", self.session_id)))
    }

    /// Claude Code's session id: `provider_session_id`, else the conversation's own id (a disk-discovered row).
    #[must_use]
    pub fn psid(&self) -> &str {
        self.provider_session_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.session_id)
    }

    /// The fingerprint of the columns the importer maps (cutover 7.4.1 `row_fp`): sha256 of their canonical JSON.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let v = serde_json::json!({
            "custom_name": self.custom_name,
            "name_source": self.name_source,
            "isArchived": self.is_archived,
            "deleted_at": self.deleted_at,
            "provider_session_id": self.provider_session_id,
            "jsonl_path": self.jsonl_path,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "model": self.model,
        });
        crate::sha_hex(v.to_string().as_bytes())
    }

    /// In Recently deleted: `"isArchived" = 1` with `deleted_at` (magpie 19.1).
    #[must_use]
    pub fn deleted(&self) -> Option<OffsetDateTime> {
        if self.is_archived != 1 {
            return None;
        }
        self.deleted_at.as_deref().and_then(parse_time)
    }
}

/// A `jarvis_failed_turns` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedTurn {
    /// Its id (the fingerprint is the sorted ids, cutover 7.4.3).
    pub id: i32,
    /// The conversation.
    pub session_id: String,
    /// The person's text, when Claude Code never got it.
    pub user_text: Option<String>,
    /// When it was sent (text).
    pub user_at: Option<String>,
    /// The sentence the old UI showed.
    pub error: String,
    /// When (text).
    pub error_at: String,
}

impl FailedTurn {
    /// Where it sits in the conversation: its send's time, else its error's.
    #[must_use]
    pub fn at(&self) -> Option<OffsetDateTime> {
        self.user_at
            .as_deref()
            .and_then(parse_time)
            .or_else(|| parse_time(&self.error_at))
    }
}

/// One person's schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonSource {
    /// `cc_u…`.
    pub schema: String,
    /// `u…` (the schema name without `cc_`), checked against the person.
    pub key: String,
    /// `users.username`, normalised (the person's email).
    pub person: String,
    /// Every sessions row of provider `claude`.
    pub sessions: Vec<SessionRow>,
    /// Failed turns by conversation id (text), ascending id.
    pub failed: BTreeMap<String, Vec<FailedTurn>>,
}

/// One read of `cloudcli`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    /// Every person's schema, by schema name.
    pub persons: Vec<PersonSource>,
    /// Schemas the importer cannot map (`plan` blockers): the schema and why.
    pub problems: Vec<(String, String)>,
    /// `cc_shared.shares`, every row (live and revoked), by id.
    pub shares: Vec<ShareRow>,
}

/// A `cc_shared.shares` row (`jarvis-shares.ts`): a frozen copy under `users/<key>/shares/<id>/`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ShareRow {
    /// 22 characters of base64url.
    pub id: String,
    /// The owner's email.
    pub owner: String,
    /// The local part of the owner's email.
    pub owner_name: String,
    /// The shared conversation's id (its sessions row).
    pub session_id: String,
    /// The title at the freeze.
    pub title: String,
    /// The frozen copy's folder in the old pods (`/jfs/users/<key>/shares/<id>`).
    pub dir: String,
    /// The files a viewer may download (canonical JSON text).
    pub files: String,
    /// When it was made (RFC 3339).
    pub created_at: String,
    /// When it was revoked (RFC 3339).
    pub revoked_at: Option<String>,
}

/// Whether `name` is a person's schema (`cc_u` + 24 hex).
#[must_use]
pub fn is_person_schema(name: &str) -> bool {
    name.strip_prefix("cc_u")
        .is_some_and(|h| h.len() == 24 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

/// Reads every person's schema in one `repeatable read`, read-only transaction (cutover 7.2).
///
/// # Errors
/// A statement error.
pub async fn read(pool: &Pool) -> Result<Snapshot> {
    aviary_pg::tx!(pool, |t| -> Result<Snapshot> {
        t.batch_execute("set transaction isolation level repeatable read, read only")
            .await?;
        let schemas: Vec<String> = t
            .query(
                "select nspname::text from pg_namespace where nspname like 'cc\\_u%' order by 1",
                &[],
            )
            .await?
            .iter()
            .map(|r| r.get(0))
            .filter(|s: &String| is_person_schema(s))
            .collect();
        let mut snap = Snapshot::default();
        for schema in schemas {
            let key = schema["cc_".len()..].to_owned();
            let users = t
                .query(&format!("select username::text from {schema}.users"), &[])
                .await?;
            let [user] = users.as_slice() else {
                snap.problems
                    .push((schema.clone(), format!("{} users rows, not one", users.len())));
                continue;
            };
            let person = aviary_core::log::normalise_email(&user.get::<_, String>(0));
            if aviary_core::log::person_key(&person) != key {
                snap.problems
                    .push((schema.clone(), "the schema's key is not its user's".to_owned()));
                continue;
            }
            let rows = t
                .query(
                    &format!(
                        "select session_id::text, provider::text, provider_session_id::text, custom_name::text,
                                name_source::text, coalesce(\"isArchived\", 0)::int4, deleted_at::text,
                                jsonl_path::text, created_at::text, updated_at::text, model::text
                         from {schema}.sessions where provider = 'claude' order by session_id"
                    ),
                    &[],
                )
                .await?;
            let sessions = rows
                .iter()
                .map(|r| SessionRow {
                    session_id: r.get(0),
                    provider: r.get(1),
                    provider_session_id: r.get(2),
                    custom_name: r.get(3),
                    name_source: r.get(4),
                    is_archived: r.get(5),
                    deleted_at: r.get(6),
                    jsonl_path: r.get(7),
                    created_at: r.get(8),
                    updated_at: r.get(9),
                    model: r.get(10),
                })
                .collect();
            let mut failed: BTreeMap<String, Vec<FailedTurn>> = BTreeMap::new();
            for r in t
                .query(
                    &format!(
                        "select id::int4, session_id::text, user_text::text, user_at::text, error::text,
                                error_at::text
                         from {schema}.jarvis_failed_turns order by id"
                    ),
                    &[],
                )
                .await?
            {
                let f = FailedTurn {
                    id: r.get(0),
                    session_id: r.get(1),
                    user_text: r.get(2),
                    user_at: r.get(3),
                    error: r.get(4),
                    error_at: r.get(5),
                };
                failed.entry(f.session_id.clone()).or_default().push(f);
            }
            snap.persons.push(PersonSource {
                schema,
                key,
                person,
                sessions,
                failed,
            });
        }
        snap.shares = shares(t).await?;
        Ok(snap)
    })
    .await
}

/// `cc_shared.shares` (none when the schema does not exist), in the snapshot's transaction.
async fn shares(t: &aviary_pg::Tx<'_>) -> Result<Vec<ShareRow>> {
    let has_shares: bool = t
        .query_one("select to_regclass('cc_shared.shares') is not null", &[])
        .await?
        .get(0);
    if !has_shares {
        return Ok(Vec::new());
    }
    Ok(t.query(
        "select id::text, owner::text, owner_name::text, session_id::text, title::text, dir::text,
                    files::text, to_char(created_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"'),
                    to_char(revoked_at at time zone 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')
             from cc_shared.shares order by id",
        &[],
    )
    .await?
    .iter()
    .map(|r| ShareRow {
        id: r.get(0),
        owner: r.get(1),
        owner_name: r.get(2),
        session_id: r.get(3),
        title: r.get(4),
        dir: r.get(5),
        files: r.get(6),
        created_at: r.get(7),
        revoked_at: r.get(8),
    })
    .collect())
}

/// A `CloudCLI` text timestamp (magpie 19.1): `YYYY-MM-DD HH:MM:SS` (UTC) or ISO 8601 / RFC 3339. The 1970
/// placeholder and anything unparsable are `None` (the caller uses the transcript's first record time).
#[must_use]
pub fn parse_time(s: &str) -> Option<OffsetDateTime> {
    let s = s.trim();
    let t = OffsetDateTime::parse(s, &Rfc3339).ok().or_else(|| {
        let f = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
        let f_frac = time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]:[second].[subsecond]");
        time::PrimitiveDateTime::parse(s, &f)
            .or_else(|_| time::PrimitiveDateTime::parse(s, &f_frac))
            .ok()
            .map(time::PrimitiveDateTime::assume_utc)
            .or_else(|| {
                // ISO without a zone, or with a `T` but no offset: UTC (SQLite's reading)
                let f_t = time::macros::format_description!(
                    "[year]-[month]-[day]T[hour]:[minute]:[second][optional [.[subsecond]]]"
                );
                time::PrimitiveDateTime::parse(s, &f_t)
                    .ok()
                    .map(time::PrimitiveDateTime::assume_utc)
            })
    })?;
    (t.year() > 1970).then_some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_timestamp_forms_and_the_placeholder() {
        let a = parse_time("2026-10-22 06:22:36").unwrap();
        assert_eq!(a.unix_timestamp(), 1_792_650_156);
        let b = parse_time("2026-10-22T02:07:21.737Z").unwrap();
        assert_eq!(b.unix_timestamp(), 1_792_634_841);
        assert_eq!(b.millisecond(), 737);
        assert!(parse_time("1970-01-01T00:00:00.000Z").is_none());
        assert!(parse_time("1970-01-01 00:00:00").is_none());
        assert!(parse_time("").is_none());
        assert!(parse_time("garbage").is_none());
        assert_eq!(parse_time("2026-10-22T06:22:36").unwrap(), a);
    }

    #[test]
    fn person_schemas() {
        assert!(is_person_schema("cc_u0f8099ba706a214946d13a99"));
        assert!(!is_person_schema("cc_shared"));
        assert!(!is_person_schema("cc_u0f8099ba706a214946d13a9"));
        assert!(!is_person_schema("cc_u0F8099ba706a214946d13a99"));
    }
}

//! Conversations (magpie 9): the Conversation object as read from the tables, the person row, the row lock every
//! writer takes, and the routes' operations (create, list, rename, delete, restore, purge).

pub mod create;
pub mod delete;
pub mod list;
pub mod rename;

use std::collections::HashMap;

use aviary_pg::Row;
use magpie_api::Timestamp;
use magpie_api::conversations::{
    Ask, BackgroundTask, Conversation, LastTurn, Origin, OriginKind, RunInfo, RunState, SandboxPhase, TitleSource,
    TurnRef,
};
use magpie_api::errors::{DataCode, ErrorCode, ErrorInfo, Refusal};
use magpie_api::events::AskPending;
use magpie_api::server::{Ctx, Viewer};
use magpie_api::turns::{TurnOrigin, TurnOutcome};
use serde_json::Value;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::db::Db;
use crate::error::{Error, Result};
use crate::limits;

/// The person a pigeon request acts for, and the admin viewing as them (magpie 3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    /// The email, `lower(btrim())`.
    pub email: String,
    /// Set while an admin views as the person.
    pub viewer: Option<Viewer>,
}

impl Person {
    /// From the request's headers (the route layer has already required `Aviary-Person` on person routes).
    ///
    /// # Errors
    /// 400 `bad_request` when the header is missing or is not an email.
    pub fn from_ctx(ctx: &Ctx) -> Result<Self> {
        let email = ctx
            .person
            .clone()
            .ok_or_else(|| Error::bad_request("Aviary-Person is required"))?;
        if email.find('@').is_none_or(|i| i < 1) {
            return Err(Error::bad_request("Aviary-Person is not an email"));
        }
        Ok(Self {
            email,
            viewer: ctx.viewer.clone(),
        })
    }

    /// The admin's email while viewing as.
    #[must_use]
    pub fn viewer_email(&self) -> Option<String> {
        self.viewer.as_ref().map(|v| v.email.clone())
    }

    /// 403 `view_as_read_only` while viewing as (rename, delete, restore, purge, shares).
    ///
    /// # Errors
    /// That refusal.
    pub fn refuse_view_as(&self) -> Result<()> {
        if self.viewer.is_some() {
            return Err(Error::code(
                ErrorCode::ViewAsReadOnly,
                "read-only while viewing as the person",
            ));
        }
        Ok(())
    }
}

/// A person's row (magpie 4).
#[derive(Debug, Clone)]
pub struct PersonRow {
    /// `'u' || left(sha256hex(person), 24)`.
    pub person_key: String,
    /// The change feed's generation.
    pub generation: Uuid,
    /// The change feed's head.
    pub change_seq: i64,
}

/// The person's row, created on first sight (magpie 3.2). Takes no row lock.
///
/// # Errors
/// When a statement fails.
pub async fn ensure_person(db: &impl Db, person: &str) -> Result<PersonRow> {
    db.execute(
        "insert into persons (person, person_key) values ($1, $2) on conflict do nothing",
        &[&person, &aviary_core::log::person_key(person)],
    )
    .await?;
    person_row(db, person).await
}

/// The person's row, which exists ([`ensure_person`] made it).
///
/// # Errors
/// When a statement fails, or there is no row.
pub async fn person_row(db: &impl Db, person: &str) -> Result<PersonRow> {
    let row = db
        .query_one(
            "select person_key, change_generation, change_seq from persons where person = $1",
            &[&person],
        )
        .await?;
    Ok(PersonRow {
        person_key: row.get(0),
        generation: row.get(1),
        change_seq: row.get(2),
    })
}

/// The columns of [`conversation_from_row`], over `conversations c` with its active and last turn.
pub const SELECT: &str = "select c.id, c.title, c.name_source, c.created_at, c.activity_at, c.last_user_message_at,
    c.deleted_at, c.provider_session_id, c.run_state, c.background, c.sandbox_phase, c.model, c.context_tokens,
    c.migrating, c.origin, c.person,
    t.id as t_id, t.created_at as t_at, t.origin as t_origin,
    l.id as l_id, l.state as l_state, l.ended_at as l_ended, l.created_at as l_created, l.error_code as l_code,
    l.error_detail as l_detail
  from conversations c
  left join turns t on t.id = c.active_turn
  left join turns l on l.id = c.last_turn";

fn ts(t: OffsetDateTime) -> Timestamp {
    Timestamp(t)
}

fn turn_origin(s: &str) -> TurnOrigin {
    match s {
        "agent" => TurnOrigin::Agent,
        "import" => TurnOrigin::Import,
        _ => TurnOrigin::Person,
    }
}

fn run_state(s: &str) -> RunState {
    match s {
        "admitted" => RunState::Admitted,
        "preparing" => RunState::Preparing,
        "running" => RunState::Running,
        "stopping" => RunState::Stopping,
        "held" => RunState::Held,
        _ => RunState::Idle,
    }
}

/// A Conversation from a row of [`SELECT`] (asks empty: [`attach_asks`] adds them).
///
/// # Errors
/// When a JSON column does not decode.
pub fn conversation_from_row(row: &Row) -> Result<Conversation> {
    let title: Option<String> = row.get("title");
    let name_source: Option<String> = row.get("name_source");
    let title_source = match (&title, name_source.as_deref()) {
        (None, _) => None,
        (Some(_), Some("title")) => Some(TitleSource::Generated),
        (Some(_), Some("user")) => Some(TitleSource::User),
        (Some(_), _) => Some(TitleSource::Provisional),
    };
    let deleted_at: Option<OffsetDateTime> = row.get("deleted_at");
    let turn = row.get::<_, Option<Uuid>>("t_id").map(|id| TurnRef {
        id,
        started_at: ts(row.get("t_at")),
        origin: turn_origin(row.get("t_origin")),
        ask_wait_ms: 0,
    });
    let last_turn = match row.get::<_, Option<Uuid>>("l_id") {
        None => None,
        Some(id) => {
            let state: String = row.get("l_state");
            let outcome = match state.as_str() {
                "failed" => TurnOutcome::Failed,
                "canceled" => TurnOutcome::Canceled,
                _ => TurnOutcome::Completed,
            };
            let ended: Option<OffsetDateTime> = row.get("l_ended");
            let error = row.get::<_, Option<String>>("l_code").map(|code| {
                ErrorInfo::new(
                    DataCode::from_wire(&code),
                    row.get::<_, Option<String>>("l_detail").unwrap_or_default(),
                )
            });
            Some(LastTurn {
                id,
                outcome,
                ended_at: ts(ended.unwrap_or_else(|| row.get("l_created"))),
                error,
            })
        }
    };
    let background: Value = row.get("background");
    let background: Vec<BackgroundTask> = serde_json::from_value(background).unwrap_or_default();
    let origin: Value = row.get("origin");
    let origin: Origin = serde_json::from_value(origin).unwrap_or(Origin {
        kind: OriginKind::New,
        share_id: None,
    });
    let sandbox = match row.get::<_, Option<String>>("sandbox_phase").as_deref() {
        Some("preparing") => Some(SandboxPhase::Preparing),
        Some("ready") => Some(SandboxPhase::Ready),
        _ => None,
    };
    Ok(Conversation {
        id: row.get("id"),
        title,
        title_source,
        created_at: ts(row.get("created_at")),
        updated_at: ts(row.get("activity_at")),
        last_user_message_at: row.get::<_, Option<OffsetDateTime>>("last_user_message_at").map(ts),
        deleted_at: deleted_at.map(ts),
        purge_at: deleted_at.map(|d| ts(d + limits::RECENTLY_DELETED)),
        claude_session_id: row.get("provider_session_id"),
        turn,
        last_turn,
        sandbox,
        asks: Vec::new(),
        migrating: row.get("migrating"),
        run: RunInfo {
            state: run_state(row.get("run_state")),
            background,
        },
        model: row.get("model"),
        context_tokens: row
            .get::<_, Option<i32>>("context_tokens")
            .and_then(|n| u64::try_from(n).ok()),
        origin,
    })
}

/// Fills `asks` of the conversations that have a turn: the `ask.pending` events not followed by their
/// `ask.resolved`; and the turn's `askWaitMs`, the time its answered questions waited (DECISIONS 17).
///
/// # Errors
/// When a statement fails.
pub async fn attach_asks(db: &impl Db, convs: &mut [Conversation]) -> Result<()> {
    let ids: Vec<Uuid> = convs.iter().filter(|c| c.turn.is_some()).map(|c| c.id).collect();
    if ids.is_empty() {
        return Ok(());
    }
    let rows = db
        .query(
            "select e.conversation_id, e.data, e.at from events e
             where e.conversation_id = any($1) and e.kind = 'ask.pending'
               and not exists (select 1 from events r
                               where r.conversation_id = e.conversation_id and r.kind = 'ask.resolved'
                                 and r.data->>'askId' = e.data->>'askId')
             order by e.conversation_id, e.seq",
            &[&ids],
        )
        .await?;
    let mut by_conv: HashMap<Uuid, Vec<Ask>> = HashMap::new();
    for r in rows {
        let data: Value = r.get(1);
        let Ok(p) = serde_json::from_value::<AskPending>(data) else {
            continue;
        };
        by_conv.entry(r.get(0)).or_default().push(Ask {
            id: p.ask_id,
            tool_name: p.tool_name,
            tool_use_id: p.tool_use_id,
            input: p.input,
            title: None,
            description: None,
            created_at: ts(r.get(2)),
        });
    }
    let turns: Vec<Uuid> = convs.iter().filter_map(|c| c.turn.as_ref().map(|t| t.id)).collect();
    let waits = crate::turns::parked::ask_waits(db, &ids, &turns).await?;
    for c in convs {
        if let Some(asks) = by_conv.remove(&c.id) {
            c.asks = asks;
        }
        if let Some(t) = c.turn.as_mut() {
            t.ask_wait_ms = waits.get(&t.id).copied().unwrap_or(0);
        }
    }
    Ok(())
}

/// The conversation `id` (deleted ones included), if it exists.
///
/// # Errors
/// When a statement fails.
pub async fn load(db: &impl Db, id: Uuid) -> Result<Option<(String, Conversation)>> {
    let Some(row) = db.query_opt(&format!("{SELECT} where c.id = $1"), &[&id]).await? else {
        return Ok(None);
    };
    let person: String = row.get("person");
    let mut c = [conversation_from_row(&row)?];
    attach_asks(db, &mut c).await?;
    let [c] = c;
    Ok(Some((person, c)))
}

/// The person's conversation `id`; 404 `not_found` when it does not exist or is someone else's (magpie 3.2).
///
/// # Errors
/// That refusal, or a statement error.
pub async fn load_owned(db: &impl Db, person: &str, id: Uuid) -> Result<Conversation> {
    match load(db, id).await? {
        Some((owner, c)) if owner == person => Ok(c),
        _ => Err(Error::not_found()),
    }
}

/// The conversations `ids`, in the given order (missing ones left out).
///
/// # Errors
/// When a statement fails.
pub async fn load_many(db: &impl Db, ids: &[Uuid]) -> Result<Vec<Conversation>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids_vec = ids.to_vec();
    let rows = db.query(&format!("{SELECT} where c.id = any($1)"), &[&ids_vec]).await?;
    let mut by_id = HashMap::new();
    for r in &rows {
        let c = conversation_from_row(r)?;
        by_id.insert(c.id, c);
    }
    let mut out: Vec<Conversation> = ids.iter().filter_map(|id| by_id.remove(id)).collect();
    attach_asks(db, &mut out).await?;
    Ok(out)
}

/// What a writer needs to know about the conversation it has locked.
#[derive(Debug, Clone)]
pub struct Locked {
    /// The conversation.
    pub id: Uuid,
    /// Its owner.
    pub person: String,
    /// In Recently deleted since.
    pub deleted_at: Option<OffsetDateTime>,
    /// The importer holds it.
    pub migrating: bool,
    /// A reverse hand-over has fenced it (owned, cutover 14.2): its writes wait ([`crate::legacy::fence::native`]).
    pub fenced: bool,
}

/// Locks the person's conversation row (`select … for update`, magpie 4: always the conversation before the
/// person); 404 `not_found` when it does not exist or is someone else's.
///
/// # Errors
/// That refusal, or a statement error.
pub async fn lock(db: &impl Db, person: &str, id: Uuid) -> Result<Locked> {
    let row = db
        .query_opt(
            &format!(
                "select person, deleted_at, migrating, {} from conversations where id = $1 for update",
                crate::legacy::fence::REVERSE_FENCED_SQL
            ),
            &[&id],
        )
        .await?
        .ok_or_else(Error::not_found)?;
    let owner: String = row.get(0);
    if owner != person {
        return Err(Error::not_found());
    }
    Ok(Locked {
        id,
        person: owner,
        deleted_at: row.get(1),
        migrating: row.get(2),
        fenced: row.get(3),
    })
}

impl Locked {
    /// 409 `conversation_migrating` `route: "legacy"` for a migrating conversation (cutover 6.4, DESIGN 12 D14):
    /// its writes belong to the old stack until the importer releases it. A fenced conversation's wait for the
    /// release is the window routes' (WP-51). An owned conversation under a reverse fence (cutover 14.2) answers 409
    /// `conversation_migrating` without a route: the window routes wait for its end, then run natively or follow the
    /// reopen's 409 `route: "legacy"`.
    ///
    /// # Errors
    /// That refusal.
    pub fn refuse_migrating(&self) -> Result<()> {
        if self.migrating {
            let mut r = Refusal::legacy_route();
            r.retry_after_ms = Some(limits::MIGRATING_RETRY_AFTER_MS);
            return Err(r.into());
        }
        if self.fenced {
            return Err(Refusal::migrating(limits::MIGRATING_RETRY_AFTER_MS).into());
        }
        Ok(())
    }
}

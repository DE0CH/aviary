//! Schedules (Deyao, 2026-11-06; magpie-api [`magpie_api::schedules`]): a prompt the conversation's AI arms
//! to be posted into that same conversation once (`at`) or on a repeating rule (`cron` in `tz`, optionally `until`).
//!
//! The AI arms and changes them through the sandbox routes (the person and the conversation are the gateway's
//! verified sandbox identity: it can arm only for its own person, only in its own conversation); the app reads one
//! conversation's for its panel; an admin reads everyone's and holds the pause-all switch. Firing is [`fire`]'s, on
//! the job leader. Safety stops only (not budgets): a repeating rule fires at most every 15 minutes, and a person
//! has at most [`MAX_LIVE`] schedules that are not done or deleted.

pub mod cron;
pub mod fire;

use std::time::Duration;

use aviary_pg::Pool;
use magpie_api::schedules::{
    AdminSchedules, CreateSchedule, PauseAll, Schedule, ScheduleRun, SchedulesList, UpdateSchedule,
};
use magpie_api::server::Ctx;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::conversations::Person;
use crate::db::Db;
use crate::error::{Error, Result};
use crate::turns::caller::Sandbox;

/// The shortest time between two fires of a repeating rule.
pub const MIN_GAP: Duration = Duration::from_mins(15);
/// Schedules a person may have that are not done or deleted.
pub const MAX_LIVE: i64 = 25;
/// A prompt's length, characters.
pub const PROMPT_MAX_CHARS: usize = 8000;
/// How late a fire may still go out after the service was down (Deyao, 2026-11-06: later, it is skipped).
pub const LATE_LIMIT: Duration = Duration::from_mins(10);
/// Runs shown per schedule.
pub const RUNS_SHOWN: i64 = 10;
/// How far ahead a one-off may be set.
const AT_MAX_AHEAD: i64 = 2 * 366 * 86_400;

/// The columns [`Row::of`] reads.
const COLUMNS: &str = "s.id, s.person, s.conversation_id, s.kind, s.prompt, s.at, s.cron, s.tz, s.until, s.describe, \
                       s.next_at, s.state, s.created_at, s.created_by, c.title";

/// A schedule row.
#[derive(Debug, Clone)]
pub struct Row {
    /// `sch_…`.
    pub id: String,
    /// The owner.
    pub person: String,
    /// The conversation.
    pub conversation: Uuid,
    /// `once` or `cron`.
    pub kind: String,
    /// The message.
    pub prompt: String,
    /// A one-off's time.
    pub at: Option<OffsetDateTime>,
    /// A repeating rule.
    pub cron: Option<String>,
    /// The zone.
    pub tz: String,
    /// A repeating rule's end.
    pub until: Option<OffsetDateTime>,
    /// The reading.
    pub describe: String,
    /// The next fire.
    pub next_at: Option<OffsetDateTime>,
    /// `active`, `paused`, `done`, `deleted`.
    pub state: String,
    /// Set at.
    pub created_at: OffsetDateTime,
    /// Set by.
    pub created_by: String,
    /// The conversation's title.
    pub title: Option<String>,
}

impl Row {
    fn of(r: &aviary_pg::Row) -> Self {
        Self {
            id: r.get(0),
            person: r.get(1),
            conversation: r.get(2),
            kind: r.get(3),
            prompt: r.get(4),
            at: r.get(5),
            cron: r.get(6),
            tz: r.get(7),
            until: r.get(8),
            describe: r.get(9),
            next_at: r.get(10),
            state: r.get(11),
            created_at: r.get(12),
            created_by: r.get(13),
            title: r.get(14),
        }
    }

    fn zone(&self) -> chrono_tz::Tz {
        cron::zone(&self.tz).unwrap_or(chrono_tz::Asia::Hong_Kong)
    }

    fn local(&self, t: OffsetDateTime) -> String {
        cron::local(t.unix_timestamp(), self.zone())
    }

    /// The contract's form, with `runs` when given.
    #[must_use]
    pub fn api(&self, runs: Option<Vec<ScheduleRun>>, title: bool) -> Schedule {
        Schedule {
            id: self.id.clone(),
            conversation_id: self.conversation,
            kind: self.kind.clone(),
            prompt: self.prompt.clone(),
            at: self.at.map(|t| self.local(t)),
            cron: self.cron.clone(),
            tz: self.tz.clone(),
            until: self.until.map(|t| self.local(t)),
            describe: self.describe.clone(),
            next: self.next_at.map(|t| self.local(t)),
            state: self.state.clone(),
            created_by: self.created_by.clone(),
            created_at: self.local(self.created_at),
            conversation_title: if title { self.title.clone() } else { None },
            runs,
        }
    }
}

/// What a request sets: the rule, checked, with its reading and next fire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    /// `once` or `cron`.
    pub kind: &'static str,
    /// A one-off's time (Unix seconds).
    pub at: Option<i64>,
    /// A repeating rule, normalised (single spaces).
    pub cron: Option<String>,
    /// The zone.
    pub tz: String,
    /// The end (Unix seconds).
    pub until: Option<i64>,
    /// The reading.
    pub describe: String,
    /// The next fire (Unix seconds).
    pub next: i64,
}

/// Checks a rule and computes its reading and next fire at `now`.
///
/// # Errors
/// 400 `bad_request` saying what is wrong, in words the AI can repeat.
pub fn spec(at: Option<&str>, rule: Option<&str>, tz: Option<&str>, until: Option<&str>, now: i64) -> Result<Spec> {
    let tz_name = tz.map(str::trim).filter(|t| !t.is_empty()).unwrap_or(cron::DEFAULT_TZ);
    let zone = cron::zone(tz_name).map_err(Error::bad_request)?;
    let at = at.map(str::trim).filter(|a| !a.is_empty());
    let rule = rule.map(str::trim).filter(|c| !c.is_empty());
    let until = until.map(str::trim).filter(|u| !u.is_empty());
    match (at, rule) {
        (Some(_), Some(_)) => Err(Error::bad_request(
            "give either at (once) or cron (repeating), not both",
        )),
        (None, None) => Err(Error::bad_request(
            "give at (a one-off time like 2026-11-07T09:00+08:00) or cron (five fields, like 30 8 * * 1-5)",
        )),
        (Some(a), None) => {
            if until.is_some() {
                return Err(Error::bad_request("until is only for a repeating schedule (cron)"));
            }
            let t = cron::parse_time(a, zone).map_err(Error::bad_request)?;
            if t < now - 60 {
                return Err(Error::bad_request(format!(
                    "{} is in the past (now it is {})",
                    cron::local(t, zone),
                    cron::local(now, zone)
                )));
            }
            if t > now + AT_MAX_AHEAD {
                return Err(Error::bad_request("a one-off can be at most two years ahead"));
            }
            Ok(Spec {
                kind: "once",
                at: Some(t),
                cron: None,
                tz: tz_name.to_owned(),
                until: None,
                describe: cron::describe_once(t, zone, now),
                next: t.max(now),
            })
        }
        (None, Some(c)) => {
            let expr = c.split_whitespace().collect::<Vec<_>>().join(" ");
            let r = cron::parse(&expr).map_err(Error::bad_request)?;
            let until = until
                .map(|u| cron::parse_time(u, zone).map_err(Error::bad_request))
                .transpose()?;
            if until.is_some_and(|u| u <= now) {
                return Err(Error::bad_request("until is in the past"));
            }
            let next = r
                .next_after(zone, now)
                .filter(|n| until.is_none_or(|u| *n <= u))
                .ok_or_else(|| Error::bad_request(format!("`{expr}` never fires (before until)")))?;
            let min = i64::try_from(MIN_GAP.as_secs()).unwrap_or(900);
            if r.min_gap(zone, now, 200).is_some_and(|g| g < min) {
                return Err(Error::bad_request(
                    "a repeating schedule fires at most every 15 minutes",
                ));
            }
            Ok(Spec {
                kind: "cron",
                at: None,
                describe: cron::describe_cron(&r, &expr, tz_name),
                cron: Some(expr),
                tz: tz_name.to_owned(),
                until,
                next,
            })
        }
    }
}

fn instant(t: i64) -> Result<OffsetDateTime> {
    OffsetDateTime::from_unix_timestamp(t).map_err(|_| Error::bad_request("the time is out of range"))
}

fn check_prompt(p: &str) -> Result<()> {
    if p.trim().is_empty() {
        return Err(Error::bad_request("prompt is empty: give the exact message to post"));
    }
    if p.chars().count() > PROMPT_MAX_CHARS {
        return Err(Error::bad_request("prompt is over 8000 characters"));
    }
    Ok(())
}

/// The last runs of each schedule in `ids`, newest first.
async fn runs(db: &impl Db, ids: &[String], zones: &[(String, chrono_tz::Tz)]) -> Result<Vec<(String, ScheduleRun)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    // a delivered run reads as answered once its turn completed (the panel's 已回覆), unanswered once it failed or was
    // canceled; until then it is `sent` / `sent_mid_turn` (migration 0027; a carried-over run has no turn and keeps
    // what the old stack recorded)
    let rows = db
        .query(
            "select x.schedule_id, x.due_at, x.at,
                    case when x.outcome in ('sent', 'sent_mid_turn') and t.state = 'completed'
                           then case x.outcome when 'sent' then 'answered' else 'answered_mid_turn' end
                         when x.outcome in ('sent', 'sent_mid_turn') and t.state in ('failed', 'canceled')
                           then 'unanswered'
                         else x.outcome end,
                    x.detail
             from (select r.*, row_number() over (partition by schedule_id order by due_at desc) as n
                   from schedule_runs r where schedule_id = any($1)) x
             left join turns t on t.id = x.turn_id
             where x.n <= $2 order by x.schedule_id, x.due_at desc",
            &[&ids, &RUNS_SHOWN],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let id: String = r.get(0);
            let zone = zones
                .iter()
                .find(|(i, _)| *i == id)
                .map_or(chrono_tz::Asia::Hong_Kong, |(_, z)| *z);
            let due: OffsetDateTime = r.get(1);
            let at: OffsetDateTime = r.get(2);
            let run = ScheduleRun {
                due: cron::local(due.unix_timestamp(), zone),
                at: cron::local(at.unix_timestamp(), zone),
                outcome: r.get(3),
                detail: r.get(4),
            };
            (id, run)
        })
        .collect())
}

/// Each row with its runs.
async fn with_runs(db: &impl Db, rows: Vec<Row>, title: bool) -> Result<Vec<Schedule>> {
    let ids: Vec<String> = rows.iter().map(|r| r.id.clone()).collect();
    let zones: Vec<(String, chrono_tz::Tz)> = rows.iter().map(|r| (r.id.clone(), r.zone())).collect();
    let all = runs(db, &ids, &zones).await?;
    Ok(rows
        .iter()
        .map(|r| {
            let mine: Vec<ScheduleRun> = all
                .iter()
                .filter(|(id, _)| *id == r.id)
                .map(|(_, run)| run.clone())
                .collect();
            r.api(Some(mine), title)
        })
        .collect())
}

fn new_id() -> String {
    format!("sch_{}", Uuid::new_v4().simple())
}

/// `POST /v1/sandbox/schedules`.
///
/// # Errors
/// 400 for a rule, prompt or conversation that cannot have one; a statement error.
pub async fn create(pool: &Pool, ctx: &Ctx, req: &CreateSchedule) -> Result<Schedule> {
    let who = Sandbox::from_ctx(ctx)?;
    check_prompt(&req.prompt)?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let s = spec(
        req.at.as_deref(),
        req.cron.as_deref(),
        req.tz.as_deref(),
        req.until.as_deref(),
        now,
    )?;
    let id = new_id();
    let (at, until, next) = (
        s.at.map(instant).transpose()?,
        s.until.map(instant).transpose()?,
        instant(s.next)?,
    );
    aviary_pg::tx!(pool, |t| -> Result<Schedule> {
        let c = crate::conversations::lock(t, &who.person, who.conversation).await?;
        if c.deleted_at.is_some() {
            return Err(Error::bad_request("this conversation is in Recently deleted"));
        }
        let live: i64 = t
            .query_one(
                "select count(*) from schedules where person = $1 and state in ('active', 'paused')",
                &[&who.person],
            )
            .await?
            .get(0);
        if live >= MAX_LIVE {
            return Err(Error::bad_request(format!(
                "this person already has {MAX_LIVE} schedules: delete or finish one first"
            )));
        }
        t.execute(
            "insert into schedules (id, person, conversation_id, kind, prompt, at, cron, tz, until, describe, next_at,
                                    state, created_by)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'active', $2)",
            &[
                &id,
                &who.person,
                &who.conversation,
                &s.kind,
                &req.prompt,
                &at,
                &s.cron,
                &s.tz,
                &until,
                &s.describe,
                &next,
            ],
        )
        .await?;
        tracing::info!(schedule = %id, conversation = %who.conversation, kind = s.kind, "schedule armed");
        let row = one(t, &id, &who.person).await?.ok_or_else(Error::not_found)?;
        Ok(row.api(None, false))
    })
    .await
}

async fn one(db: &impl Db, id: &str, person: &str) -> Result<Option<Row>> {
    Ok(db
        .query_opt(
            &format!(
                "select {COLUMNS} from schedules s join conversations c on c.id = s.conversation_id
                 where s.id = $1 and s.person = $2 and s.state <> 'deleted'"
            ),
            &[&id, &person],
        )
        .await?
        .as_ref()
        .map(Row::of))
}

/// `GET /v1/sandbox/schedules`: the person's schedules in every conversation, newest first.
///
/// # Errors
/// A statement error.
pub async fn list(pool: &Pool, ctx: &Ctx) -> Result<SchedulesList> {
    let who = Sandbox::from_ctx(ctx)?;
    let conn = pool.get().await?;
    let rows = conn
        .query(
            &format!(
                "select {COLUMNS} from schedules s join conversations c on c.id = s.conversation_id
                 where s.person = $1 and s.state <> 'deleted' order by s.created_at desc"
            ),
            &[&who.person],
        )
        .await?;
    Ok(SchedulesList {
        schedules: rows.iter().map(|r| Row::of(r).api(None, true)).collect(),
    })
}

/// `GET /v1/sandbox/schedules/{id}`: one, with its last runs.
///
/// # Errors
/// 404 when it is not the person's; a statement error.
pub async fn get(pool: &Pool, ctx: &Ctx, id: &str) -> Result<Schedule> {
    let who = Sandbox::from_ctx(ctx)?;
    let conn = pool.get().await?;
    let row = one(&conn, id, &who.person).await?.ok_or_else(Error::not_found)?;
    Ok(with_runs(&conn, vec![row], true).await?.remove(0))
}

/// `PATCH /v1/sandbox/schedules/{id}`.
///
/// # Errors
/// 404 when it is not the person's; 400 for a change that cannot be made; a statement error.
#[allow(clippy::too_many_lines)] // one change, its checks and the write
pub async fn update(pool: &Pool, ctx: &Ctx, id: &str, req: &UpdateSchedule) -> Result<Schedule> {
    let who = Sandbox::from_ctx(ctx)?;
    if let Some(p) = &req.prompt {
        check_prompt(p)?;
    }
    let now = OffsetDateTime::now_utc().unix_timestamp();
    aviary_pg::tx!(pool, |t| -> Result<Schedule> {
        let row = t
            .query_opt(
                &format!(
                    "select {COLUMNS} from schedules s join conversations c on c.id = s.conversation_id
                     where s.id = $1 and s.person = $2 and s.state <> 'deleted' for update of s"
                ),
                &[&id, &who.person],
            )
            .await?
            .as_ref()
            .map(Row::of)
            .ok_or_else(Error::not_found)?;
        let rule_changed = req.at.is_some() || req.cron.is_some() || req.tz.is_some() || req.until.is_some();
        let mut state = row.state.clone();
        if let Some(p) = req.paused {
            state = match (p, state.as_str()) {
                (true, "active") => "paused".to_owned(),
                (false, "paused") => "active".to_owned(),
                (_, s) => s.to_owned(),
            };
        }
        // the rule as it will be: the changed fields over the stored ones (a new `at` makes a one-off, a new
        // `cron` a repeating one)
        let zone = row.zone();
        let stored_at = row.at.map(|a| cron::local(a.unix_timestamp(), zone));
        let stored_until = row.until.map(|u| cron::local(u.unix_timestamp(), zone));
        let (at, rule) = match (&req.at, &req.cron) {
            (Some(a), _) => (Some(a.clone()), None),
            (None, Some(c)) => (None, Some(c.clone())),
            (None, None) => (stored_at, row.cron.clone()),
        };
        let until = match &req.until {
            Some(u) => Some(u.clone()),
            None if at.is_none() => stored_until,
            None => None,
        };
        let tz = req.tz.clone().unwrap_or_else(|| row.tz.clone());
        if rule_changed && state == "done" {
            "active".clone_into(&mut state);
        }
        let mut next = None;
        let mut s = None;
        if state == "active" {
            let checked = spec(at.as_deref(), rule.as_deref(), Some(tz.as_str()), until.as_deref(), now)?;
            next = Some(instant(checked.next)?);
            s = Some(checked);
        } else if rule_changed {
            // a paused one keeps no next fire, but the new rule is still checked
            s = Some(spec(
                at.as_deref(),
                rule.as_deref(),
                Some(tz.as_str()),
                until.as_deref(),
                now,
            )?);
        }
        let prompt = req.prompt.clone().unwrap_or_else(|| row.prompt.clone());
        match s {
            Some(s) => {
                let (at, until) = (s.at.map(instant).transpose()?, s.until.map(instant).transpose()?);
                t.execute(
                    "update schedules set prompt = $2, kind = $3, at = $4, cron = $5, tz = $6, until = $7,
                                          describe = $8, next_at = $9, state = $10, updated_at = now()
                     where id = $1",
                    &[
                        &id,
                        &prompt,
                        &s.kind,
                        &at,
                        &s.cron,
                        &s.tz,
                        &until,
                        &s.describe,
                        &next,
                        &state,
                    ],
                )
                .await?;
            }
            None => {
                t.execute(
                    "update schedules set prompt = $2, state = $3, next_at = $4, updated_at = now() where id = $1",
                    &[&id, &prompt, &state, &next],
                )
                .await?;
            }
        }
        tracing::info!(schedule = %id, state = %state, "schedule changed");
        let row = one(t, id, &who.person).await?.ok_or_else(Error::not_found)?;
        Ok(row.api(None, true))
    })
    .await
}

/// `DELETE /v1/sandbox/schedules/{id}`: it never fires again (its runs stay for the admin's view).
///
/// # Errors
/// 404 when it is not the person's; a statement error.
pub async fn delete(pool: &Pool, ctx: &Ctx, id: &str) -> Result<()> {
    let who = Sandbox::from_ctx(ctx)?;
    let n = pool
        .get()
        .await?
        .execute(
            "update schedules set state = 'deleted', next_at = null, deleted_at = now(), updated_at = now()
             where id = $1 and person = $2 and state <> 'deleted'",
            &[&id, &who.person],
        )
        .await?;
    if n == 0 {
        return Err(Error::not_found());
    }
    tracing::info!(schedule = %id, "schedule deleted");
    Ok(())
}

/// `GET /v1/conversations/{id}/schedules`: the panel's list (live ones, and ones done in the last day), each with
/// its last runs.
///
/// # Errors
/// 404 when the conversation is not the person's; a statement error.
pub async fn of_conversation(pool: &Pool, person: &Person, id: Uuid) -> Result<SchedulesList> {
    let conn = pool.get().await?;
    let owner: Option<String> = conn
        .query_opt("select person from conversations where id = $1", &[&id])
        .await?
        .map(|r| r.get(0));
    if owner.as_deref() != Some(person.email.as_str()) {
        return Err(Error::not_found());
    }
    let rows = conn
        .query(
            &format!(
                "select {COLUMNS} from schedules s join conversations c on c.id = s.conversation_id
                 where s.conversation_id = $1
                   and (s.state in ('active', 'paused') or (s.state = 'done' and s.updated_at > now() - interval '1 day'))
                 order by s.created_at"
            ),
            &[&id],
        )
        .await?;
    let rows: Vec<Row> = rows.iter().map(Row::of).collect();
    Ok(SchedulesList {
        schedules: with_runs(&conn, rows, false).await?,
    })
}

async fn admin_view(db: &impl Db) -> Result<AdminSchedules> {
    let s = db
        .query_one(
            "select paused_all, paused_by, paused_at from schedule_settings where id = 1",
            &[],
        )
        .await?;
    let rows = db
        .query(
            &format!(
                "select {COLUMNS} from schedules s join conversations c on c.id = s.conversation_id
                 where s.state <> 'deleted' order by s.person, s.created_at desc"
            ),
            &[],
        )
        .await?;
    let rows: Vec<Row> = rows.iter().map(Row::of).collect();
    let paused_at: Option<OffsetDateTime> = s.get(2);
    Ok(AdminSchedules {
        paused_all: s.get(0),
        paused_by: s.get(1),
        paused_at: paused_at.map(|t| magpie_api::Timestamp(t).to_wire()),
        schedules: with_runs(db, rows, true).await?,
    })
}

/// `GET /v1/admin/schedules`.
///
/// # Errors
/// 400 without `Aviary-Admin`; a statement error.
pub async fn admin_list(pool: &Pool, ctx: &Ctx) -> Result<AdminSchedules> {
    crate::admin::admin_of(ctx)?;
    admin_view(&pool.get().await?).await
}

/// `PUT /v1/admin/schedules/pause`.
///
/// # Errors
/// 400 without `Aviary-Admin`; a statement error.
pub async fn admin_pause(pool: &Pool, ctx: &Ctx, req: &PauseAll) -> Result<AdminSchedules> {
    let admin = crate::admin::admin_of(ctx)?;
    let conn = pool.get().await?;
    conn.execute(
        "update schedule_settings set paused_all = $1, paused_by = $2, paused_at = now() where id = 1",
        &[&req.paused, &admin],
    )
    .await?;
    tracing::info!(paused = req.paused, "schedules: pause-all set");
    admin_view(&conn).await
}

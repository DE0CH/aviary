//! Failed turns (cutover 7.4.3, magpie 19.1): each `jarvis_failed_turns` row not imported yet becomes an imported
//! turn that failed: `user.sent` (when the row kept the person's text, which no transcript holds) and `turn.ended`
//! failed, with the old sentence mapped back to its code (an unknown sentence is `sandbox.failed` with the sentence
//! as its message). On the first import they are placed after every transcript event not later than their time
//! (the transcript is written up to there first, [`crate::sync::Breaks`]); in later passes they are appended.
//! The fingerprint is the sorted ids, comma-separated.

use std::collections::BTreeSet;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::Path;

use magpie::events::{self, NewEvent};
use magpie_api::errors::{DataCode, ErrorInfo};
use magpie_api::events::{EventBody, TurnEnded, UserSent};
use magpie_api::turns::TurnOutcome;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::source::{FailedTurn, parse_time};
use crate::{Error, Importer, NS, Result};

/// The old UI's sentences (`server/modules/jarvis/jarvis-errors.ts`, both locales) and their codes. `model.refused`
/// is a prefix (the provider's words follow it).
const SENTENCES: &[(&str, DataCode)] = &[
    (
        "Could not reach the computer for this conversation. Please try again in a minute.",
        DataCode::SandboxUnreachable,
    ),
    ("暫時連接不到這個對話的電腦，請稍後再試。", DataCode::SandboxUnreachable),
    (
        "All computers are busy right now. Please try again in a few minutes.",
        DataCode::SandboxBusy,
    ),
    ("所有電腦正在使用中，請過幾分鐘再試。", DataCode::SandboxBusy),
    (
        "This account is not allowed to use this service. Please ask your administrator.",
        DataCode::SandboxNotAllowed,
    ),
    ("這個帳戶不可使用此服務，請聯絡管理員。", DataCode::SandboxNotAllowed),
    (
        "Could not start the computer for this conversation. Please try again; if it keeps happening, tell your administrator.",
        DataCode::SandboxFailed,
    ),
    (
        "未能啟動這個對話的電腦，請再試一次；如問題持續，請通知管理員。",
        DataCode::SandboxFailed,
    ),
    (
        "The usage budget for this service has been used up. Please ask your administrator.",
        DataCode::ModelBudget,
    ),
    ("此服務的使用預算已用完，請聯絡管理員。", DataCode::ModelBudget),
    (
        "Too many requests to the AI model right now. Please wait a moment and try again.",
        DataCode::ModelRateLimited,
    ),
    (
        "目前向 AI 模型發出的請求太多，請稍等一會再試。",
        DataCode::ModelRateLimited,
    ),
    (
        "The AI model is busy or unavailable right now. Please try again in a moment.",
        DataCode::ModelUnavailable,
    ),
    (
        "AI 模型目前繁忙或暫時無法使用，請稍後再試。",
        DataCode::ModelUnavailable,
    ),
    (
        "The AI model service refused access. Please tell your administrator.",
        DataCode::ModelAccess,
    ),
    ("AI 模型服務拒絕存取，請通知管理員。", DataCode::ModelAccess),
    (
        "Could not reach the AI model. Please try again.",
        DataCode::ModelConnection,
    ),
    ("連接不到 AI 模型，請再試一次。", DataCode::ModelConnection),
];

const REFUSED: &[&str] = &["The AI model refused this request", "AI 模型拒絕了這個請求"];

/// The error a failed turn's sentence stands for.
#[must_use]
pub fn error_of(sentence: &str) -> ErrorInfo {
    let s = sentence.trim();
    if let Some((_, code)) = SENTENCES.iter().find(|(t, _)| *t == s) {
        return ErrorInfo::new(code.clone(), s);
    }
    for p in REFUSED {
        if let Some(rest) = s.strip_prefix(p) {
            let words = rest.trim_start_matches([':', '：', ' ']).trim();
            return ErrorInfo::new(DataCode::ModelRefused, words);
        }
    }
    ErrorInfo::new(DataCode::SandboxFailed, s)
}

/// The old UI's sentence for a failed turn's error (the reverse export, cutover 14.3): the Traditional Chinese one
/// staff read, `model.refused` with the provider's words after it, and an unknown code's own message.
#[must_use]
pub fn sentence_of(code: &str, detail: Option<&str>) -> String {
    if code == DataCode::ModelRefused.as_str() {
        return match detail.filter(|d| !d.is_empty()) {
            Some(d) => format!("{}：{d}", REFUSED[1]),
            None => REFUSED[1].to_owned(),
        };
    }
    if let Some((t, _)) = SENTENCES.iter().find(|(t, c)| c.as_str() == code && !t.is_ascii()) {
        return (*t).to_owned();
    }
    detail.filter(|d| !d.is_empty()).map_or_else(
        || "未能啟動這個對話的電腦，請再試一次；如問題持續，請通知管理員。".to_owned(),
        str::to_owned,
    )
}

/// The fingerprint of a set of failed-turn ids.
#[must_use]
pub fn fingerprint(ids: &BTreeSet<i32>) -> String {
    ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(",")
}

/// The ids a fingerprint names.
#[must_use]
pub fn ids_of(fp: &str) -> BTreeSet<i32> {
    fp.split(',').filter_map(|s| s.trim().parse().ok()).collect()
}

/// Writes one failed turn into migrating conversation `id` of `person`.
///
/// # Errors
/// [`Error::Released`] when it is not migrating; a statement error.
pub async fn insert(imp: &Importer, person: &str, id: Uuid, f: &FailedTurn) -> Result<()> {
    let turn = Uuid::new_v5(&NS, format!("failed:{id}:{}", f.id).as_bytes());
    let at = f.at().unwrap_or_else(OffsetDateTime::now_utc);
    let error = error_of(&f.error);
    let code = error.code.as_str().to_owned();
    let cmid = format!("import-failed-{}", f.id);
    let pool = &imp.magpie.pool;
    aviary_pg::tx!(pool, |t| -> Result<()> {
        let Some(r) = t
            .query_opt(
                "select migrating, person from conversations where id = $1 for update",
                &[&id],
            )
            .await?
        else {
            return Err(Error::Released);
        };
        if r.get::<_, String>(1) != person {
            return Err(crate::conv::another_persons(id));
        }
        if !r.get::<_, bool>(0) {
            return Err(Error::Released);
        }
        if t.query_opt("select 1 from turns where id = $1", &[&turn])
            .await?
            .is_some()
        {
            return Ok(());
        }
        t.execute(
            "insert into turns (id, conversation_id, origin, state, client_message_id, created_at, ended_at,
                                error_code, error_detail)
             values ($1, $2, 'import', 'failed', $3, $4, $4, $5, $6)",
            &[&turn, &id, &cmid, &at, &code, &error.message],
        )
        .await?;
        let mut user_seq = None;
        if let Some(text) = f.user_text.as_deref().filter(|t| !t.is_empty()) {
            let body = EventBody::UserSent(UserSent {
                client_message_id: cmid.clone(),
                text: text.to_owned(),
                files: Vec::new(),
                images: Vec::new(),
            });
            let mut ev = NewEvent::of(&body)?.turn(Some(turn)).text(Some(text.to_owned()));
            ev.is_prompt = true;
            ev.at = f.user_at.as_deref().and_then(parse_time).unwrap_or(at);
            user_seq = Some(events::append(t, id, ev).await?);
        }
        let body = EventBody::TurnEnded(TurnEnded {
            turn_id: turn,
            outcome: TurnOutcome::Failed,
            error: Some(error.clone()),
        });
        let mut ev = NewEvent::of(&body)?.turn(Some(turn));
        ev.at = parse_time(&f.error_at).unwrap_or(at);
        let end_seq = events::append(t, id, ev).await?;
        t.execute(
            "update turns set user_seq = $2, end_seq = $3 where id = $1",
            &[&turn, &user_seq, &end_seq],
        )
        .await?;
        t.execute(
            "update conversations set last_turn = $2, last_user_message_at = greatest(last_user_message_at, $3)
             where id = $1",
            &[&id, &turn, &user_seq.map(|_| at)],
        )
        .await?;
        magpie::changes::upserted(t, person, id).await?;
        Ok(())
    })
    .await
}

/// The record times of a transcript's lines: (offset of the line's start, its `timestamp`), complete lines only.
///
/// # Errors
/// An I/O error.
pub fn line_times(abs: &Path, size: u64) -> Result<Vec<(u64, Option<OffsetDateTime>)>> {
    #[derive(serde::Deserialize)]
    struct Ts {
        timestamp: Option<String>,
    }
    let mut r = BufReader::new(std::fs::File::open(abs)?.take(size));
    let mut out = Vec::new();
    let mut off = 0u64;
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = r.read_until(b'\n', &mut line)?;
        if n == 0 || !line.ends_with(b"\n") {
            break;
        }
        let t = serde_json::from_slice::<Ts>(&line)
            .ok()
            .and_then(|t| t.timestamp)
            .and_then(|s| parse_time(&s));
        out.push((off, t));
        off += n as u64;
    }
    Ok(out)
}

/// The failed turns to place in a new main transcript, as [`crate::sync::Breaks`]: each goes before the first line
/// whose time is later than its own (or at the end).
#[derive(Debug)]
pub struct Placement {
    person: String,
    conversation: Uuid,
    turns: Vec<FailedTurn>,
    /// Indices into `turns` per break, in order.
    groups: Vec<Vec<usize>>,
    /// Failed turns placed (the rest are appended after the transcript).
    pub placed: BTreeSet<i32>,
}

impl Placement {
    /// The turns of `conversation` to place.
    #[must_use]
    pub fn new(person: &str, conversation: Uuid, turns: Vec<FailedTurn>) -> Self {
        Self {
            person: person.to_owned(),
            conversation,
            turns,
            groups: Vec::new(),
            placed: BTreeSet::new(),
        }
    }
}

impl crate::sync::Breaks for Placement {
    fn offsets(&mut self, abs: &Path, size: u64) -> Result<Vec<(u64, usize)>> {
        let times = line_times(abs, size)?;
        let mut order: Vec<usize> = (0..self.turns.len()).collect();
        order.sort_by_key(|&i| (self.turns[i].at(), self.turns[i].id));
        let mut out: Vec<(u64, usize)> = Vec::new();
        for i in order {
            let Some(at) = self.turns[i].at() else { continue };
            // the first line later than the failed turn; a turn later than every line is appended afterwards
            let Some(&(off, _)) = times.iter().find(|(_, t)| t.is_some_and(|t| t > at)) else {
                continue;
            };
            match out.last() {
                Some(&(o, g)) if o == off => self.groups[g].push(i),
                _ => {
                    self.groups.push(vec![i]);
                    out.push((off, self.groups.len() - 1));
                }
            }
        }
        Ok(out)
    }

    fn at<'s>(
        &'s mut self,
        imp: &'s Importer,
        at: usize,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 's>> {
        Box::pin(async move {
            let group = self.groups.get(at).cloned().unwrap_or_default();
            for i in group {
                let f = self.turns[i].clone();
                insert(imp, &self.person, self.conversation, &f).await?;
                self.placed.insert(f.id);
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_map_back_to_codes() {
        assert_eq!(
            error_of("暫時連接不到這個對話的電腦，請稍後再試。").code,
            DataCode::SandboxUnreachable
        );
        assert_eq!(
            error_of(" Could not reach the AI model. Please try again. ").code,
            DataCode::ModelConnection
        );
        let r = error_of("AI 模型拒絕了這個請求：content policy");
        assert_eq!(r.code, DataCode::ModelRefused);
        assert_eq!(r.message, "content policy");
        let u = error_of("something new");
        assert_eq!(u.code, DataCode::SandboxFailed);
        assert_eq!(u.message, "something new");
    }

    #[test]
    fn codes_map_to_the_old_sentences() {
        for (t, c) in SENTENCES.iter().filter(|(t, _)| !t.is_ascii()) {
            assert_eq!(sentence_of(c.as_str(), None), *t);
            assert_eq!(error_of(&sentence_of(c.as_str(), None)).code, c.clone());
        }
        let r = sentence_of(DataCode::ModelRefused.as_str(), Some("no"));
        assert_eq!(error_of(&r).code, DataCode::ModelRefused);
        assert_eq!(sentence_of("something.else", Some("words")), "words");
    }

    #[test]
    fn fingerprints_round_trip() {
        let ids: BTreeSet<i32> = [3, 1, 2].into_iter().collect();
        assert_eq!(fingerprint(&ids), "1,2,3");
        assert_eq!(ids_of("1,2,3"), ids);
        assert!(ids_of("").is_empty());
    }
}

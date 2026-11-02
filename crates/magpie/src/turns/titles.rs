//! Titles from the title model (magpie 9.4): the `title` command's prompt, the cleaning of its answer (today's
//! `cleanTitle`, exactly), and what the ack does. A rename (`name_source 'user'`) always wins; a failure counts an
//! attempt, and the next turn tries again (at most three).

use aviary_pg::Tx;
use magpie_api::events::{EventBody, NameSource, TitleChanged};
use serde_json::Value;
use uuid::Uuid;

use crate::changes;
use crate::error::Result;
use crate::events::{self, NewEvent};
use crate::limits;

/// The user message to the title model, with the first message cut to 2,000 characters.
#[must_use]
pub fn prompt(first_message: &str) -> String {
    let m: String = first_message.chars().take(limits::TITLE_PROMPT_CHARS).collect();
    format!(
        "Here is the first message of the conversation:\n\n<message>\n{m}\n</message>\n\nReply with only the title."
    )
}

/// The last text block of a Messages API response (thinking ignored).
#[must_use]
pub fn last_text(response: &Value) -> Option<&str> {
    response
        .get("content")?
        .as_array()?
        .iter()
        .rev()
        .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
        .and_then(|b| b.get("text"))
        .and_then(Value::as_str)
}

const LABELS: [&str; 3] = ["title:", "標題:", "标题:"];
const WRAPPERS: &[char] = &[
    '"', '\'', '“', '”', '‘', '’', '「', '」', '『', '』', '《', '》', '*', '#', ' ',
];

/// Cleans a title model's answer: the first non-empty line; a leading `title:`/`標題:`/`标题:` label stripped
/// (any case); trailing `.`/`。` stripped; surrounding quotes, brackets, `*`, `#` and spaces stripped; spaces
/// collapsed; at most 80 characters. `None` when nothing is left.
#[must_use]
pub fn clean(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut s = line.to_owned();
    for label in LABELS {
        if s.to_lowercase().starts_with(label) {
            s = s[label.len()..].trim_start().to_owned();
            break;
        }
    }
    let s = s.trim_end_matches(['.', '。']);
    let s = s.trim_matches(WRAPPERS);
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s: String = s.chars().take(limits::TITLE_MAX_CHARS).collect();
    let s = s.trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// The ack of a `title` command, in the ack's transaction (the conversation is locked, owned by `person`):
/// a usable title replaces a provisional name (`title.changed` and an upsert); an error or an empty title counts an
/// attempt.
///
/// # Errors
/// When a statement fails.
pub async fn on_ack(
    tx: &Tx<'_>,
    person: &str,
    conversation: Uuid,
    response: Option<&Value>,
    error: Option<&str>,
) -> Result<()> {
    let title = match (error, response) {
        (None, Some(r)) => last_text(r).and_then(clean),
        _ => None,
    };
    let Some(title) = title else {
        tx.execute(
            "update conversations set title_attempts = title_attempts + 1 where id = $1",
            &[&conversation],
        )
        .await?;
        tracing::info!(conversation = %conversation, error = ?error, "title attempt failed");
        return Ok(());
    };
    let changed = tx
        .execute(
            "update conversations set title = $2, name_source = 'title' where id = $1 and name_source is null",
            &[&conversation, &title],
        )
        .await?;
    if changed == 1 {
        let body = EventBody::TitleChanged(TitleChanged {
            title,
            name_source: Some(NameSource::Title),
        });
        events::append(tx, conversation, NewEvent::of(&body)?).await?;
        changes::upserted(tx, person, conversation).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{clean, last_text, prompt};

    #[test]
    fn titles_are_cleaned_as_today() {
        assert_eq!(
            clean("Title: \"港珠澳大橋新聞整理\"").as_deref(),
            Some("港珠澳大橋新聞整理")
        );
        assert_eq!(clean("\n\n標題: 「季度報告」。\nmore").as_deref(), Some("季度報告"));
        assert_eq!(
            clean("标题:   **Q3   sales  review**.").as_deref(),
            Some("Q3 sales review")
        );
        assert_eq!(clean("# Weekly plan").as_deref(), Some("Weekly plan"));
        assert_eq!(clean("   \n  "), None);
        assert_eq!(clean("\"\""), None);
        assert_eq!(clean(&"字".repeat(100)).unwrap().chars().count(), 80);
    }

    #[test]
    fn the_last_text_block_is_the_title() {
        let r = serde_json::json!({"content": [
            {"type": "thinking", "thinking": "hmm"},
            {"type": "text", "text": "first"},
            {"type": "text", "text": "Final title"}
        ]});
        assert_eq!(last_text(&r), Some("Final title"));
        assert_eq!(last_text(&serde_json::json!({"content": [{"type": "thinking"}]})), None);
        let p = prompt(&"a".repeat(3000));
        assert!(p.starts_with("Here is the first message of the conversation:\n\n<message>\n"));
        assert!(p.ends_with("\n</message>\n\nReply with only the title."));
        assert!(p.contains(&"a".repeat(2000)) && !p.contains(&"a".repeat(2001)));
    }
}

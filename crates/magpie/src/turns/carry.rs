//! The carry-over of a conversation that changes harness. Each harness keeps its own session, so a turn in a
//! harness that did not run the turns before it would start without them. Its `start_turn` then names the events
//! that harness has not seen ([`range`]); the supervisor reads their rows as text ([`text`], `GET
//! /v1/sandbox/earlier`) and writes it into the workspace, where the harness reads it and sums it up before it
//! answers.

use std::fmt::Write as _;

use crate::db::Db;
use magpie_api::Harness;
use magpie_api::rows::RowItem;
use magpie_api::sandbox::CarryOver;
use serde_json::Value;
use uuid::Uuid;

use crate::error::Result;

/// The most of a tool call's input and of its result the text holds (characters).
const TOOL_TEXT: usize = 2_000;

/// The file's name for the change of harness at the event `seq`.
#[must_use]
pub fn name(seq: i64) -> String {
    format!("earlier-{seq}.md")
}

/// Records that the turn whose message is the event `user_seq` runs in `picked`, and answers what that harness has
/// not seen of the conversation: `None` for the harness of the turn before, and when no row lies in between.
///
/// # Errors
/// When a statement fails.
pub async fn range(db: &impl Db, conversation: Uuid, picked: &str, user_seq: i64) -> Result<Option<CarryOver>> {
    let row = db
        .query_one(
            "select coalesce(admitted_harness, harness), carried from conversations where id = $1",
            &[&conversation],
        )
        .await?;
    let prev: String = row.get(0);
    let carried: Value = row.get(1);
    if prev == picked {
        db.execute(
            "update conversations set admitted_harness = $2 where id = $1 and admitted_harness is distinct from $2",
            &[&conversation, &picked],
        )
        .await?;
        return Ok(None);
    }
    // the harness that ran until now has seen everything before this turn's message
    db.execute(
        "update conversations
            set admitted_harness = $2, carried = carried || jsonb_build_object($3::text, $4::bigint)
          where id = $1",
        &[&conversation, &picked, &prev, &(user_seq - 1)],
    )
    .await?;
    let after = carried.get(picked).and_then(Value::as_i64).unwrap_or(0);
    let any = db
        .query_opt(
            "select 1 from rows where conversation_id = $1 and source_seqs[1] > $2 and source_seqs[1] < $3 limit 1",
            &[&conversation, &after, &user_seq],
        )
        .await?
        .is_some();
    if !any {
        return Ok(None);
    }
    Ok(Some(CarryOver {
        from: Harness::parse(&prev).unwrap_or_default(),
        name: name(user_seq),
        after: u64::try_from(after).unwrap_or(0),
        before: u64::try_from(user_seq).unwrap_or(0),
    }))
}

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}\n… (cut: {} characters in all)", s.chars().count())
}

/// One row as text; `None` for what a reader of the conversation does not need (thinking).
#[must_use]
pub fn row_text(item: &RowItem) -> Option<String> {
    Some(match item {
        RowItem::User(u) => {
            let mut s = format!("## User\n\n{}\n", u.text);
            for f in &u.files {
                let _ = write!(s, "\n(attached file: /workspace/{})", f.path.trim_start_matches('/'));
            }
            if !u.images.is_empty() {
                let _ = write!(s, "\n({} attached image(s))", u.images.len());
            }
            s
        }
        RowItem::Assistant(a) => format!("## Assistant\n\n{}\n", a.text),
        RowItem::Tool(t) => {
            let input = serde_json::to_string(&t.input).unwrap_or_default();
            let mut s = format!("### Tool call: {}\n\nInput: {}\n", t.name, cut(&input, TOOL_TEXT));
            if let Some(r) = &t.result {
                let label = if r.is_error { "Error" } else { "Result" };
                let _ = write!(s, "\n{label}:\n{}\n", cut(&r.text, TOOL_TEXT));
            }
            for f in t.sent_files.iter().flatten() {
                let _ = write!(s, "\n(sent to the user: {})", f.path);
            }
            s
        }
        RowItem::Error(e) => format!("## Error\n\n{}\n", e.message),
        RowItem::Reasoning(_) | RowItem::Unknown => return None,
    })
}

/// The rows made from the events after `after` and before `before`, in order, as one text.
///
/// # Errors
/// When the statement fails.
pub async fn text(db: &impl Db, conversation: Uuid, after: i64, before: i64) -> Result<String> {
    let rows = db
        .query(
            "select item from rows where conversation_id = $1 and source_seqs[1] > $2 and source_seqs[1] < $3
             order by row_id",
            &[&conversation, &after, &before],
        )
        .await?;
    let mut out = String::from(
        "# Earlier conversation\n\nThe conversation so far, in order: the user's messages, the assistant's replies, \
         and the tool calls with their results (long ones cut).\n",
    );
    for r in rows {
        let item: RowItem = serde_json::from_value(r.get(0)).unwrap_or(RowItem::Unknown);
        if let Some(t) = row_text(&item) {
            out.push('\n');
            out.push_str(&t);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_read_as_a_conversation() {
        let item = |v: Value| serde_json::from_value::<RowItem>(v).unwrap();
        let user = item(
            serde_json::json!({"kind": "user", "messageId": "m1", "text": "cut my video",
            "files": [{"path": "uploads/a.mp4", "name": "a.mp4", "mimeType": "video/mp4", "size": 9}]}),
        );
        let said = row_text(&user).unwrap();
        assert!(said.starts_with("## User\n\ncut my video\n"), "{said}");
        assert!(said.contains("(attached file: /workspace/uploads/a.mp4)"));
        let tool = item(
            serde_json::json!({"kind": "tool", "callId": "c1", "messageId": "m2", "blockIndex": 0,
            "name": "Bash", "input": {"command": "ls"}, "status": "completed",
            "result": {"text": "a.mp4", "truncated": false, "bytes": 5, "isError": false}}),
        );
        let ran = row_text(&tool).unwrap();
        assert!(
            ran.contains("### Tool call: Bash")
                && ran.contains(r#"{"command":"ls"}"#)
                && ran.contains("Result:\na.mp4")
        );
        let long = cut(&"字".repeat(TOOL_TEXT + 5), TOOL_TEXT);
        assert!(long.ends_with(&format!("(cut: {} characters in all)", TOOL_TEXT + 5)));
        assert_eq!(name(41), "earlier-41.md");
    }
}

//! `magpie import parity <id>…` (cutover 10.2 rehearsal 6, 7.9): magpie's timeline rows of each conversation
//! through pigeon's own Row → `PaseoRow` projection (the one pigeon serves, Traditional Chinese locale), each
//! normalised to `[kind, role, text sha256, tool name, tool status, attachment names]`. The rehearsal compares them
//! with the old side's `parity.mjs` output for the same conversation; the only differences allowed are magpie
//! 19.3's display fixes. Reads only.

use magpie_api::Timestamp;
use magpie_api::rows::{Row, RowItem};
use pigeon::paseo::mapping::PaseoProjector;
use pigeon::paseo::{ProjectCtx, RowProjector as _};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{Importer, Result};

/// One conversation's normalised render.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Render {
    /// The conversation.
    pub conversation: Uuid,
    /// `[kind, role, text sha256, tool name, tool status, attachment names]` per shown row, in row order.
    pub rows: Vec<Value>,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

/// The normalised form of one projected Paseo row's `item`.
#[must_use]
pub fn normalise(item: &Value) -> Value {
    let kind = s(item, "type").unwrap_or_default();
    let role = match kind.as_str() {
        "user_message" => Some("user"),
        "assistant_message" | "reasoning" | "tool_call" => Some("assistant"),
        _ => None,
    };
    let text = s(item, "text").or_else(|| s(item, "message"));
    let mut names: Vec<String> = Vec::new();
    for a in item.get("attachments").and_then(Value::as_array).into_iter().flatten() {
        if let Some(n) = s(a, "fileName") {
            names.push(n);
        }
    }
    let images = item.get("images").and_then(Value::as_array).map_or(0, Vec::len);
    names.extend((0..images).map(|i| format!("image {}", i + 1)));
    for f in item
        .pointer("/metadata/sentFiles")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(n) = s(f, "name") {
            names.push(n);
        }
    }
    serde_json::json!([
        kind,
        role,
        text.map(|t| crate::sha_hex(t.as_bytes())),
        s(item, "name"),
        s(item, "status"),
        names
    ])
}

fn u(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

/// magpie's rows of `id`, as M9 serves them.
async fn rows(imp: &Importer, id: Uuid) -> Result<Vec<Row>> {
    let conn = imp.magpie.pool.get().await?;
    let mut out = Vec::new();
    for r in conn
        .query(
            "select row_id, seq_end, seqs, at, turn_id, item from rows where conversation_id = $1 order by row_id",
            &[&id],
        )
        .await?
    {
        let row_id: i64 = r.get(0);
        let seq_end: i64 = r.get(1);
        let seqs: Vec<i64> = r.get(2);
        let item: RowItem = serde_json::from_value(r.get(5))?;
        out.push(Row {
            seq_start: u(row_id),
            seq_end: u(seq_end),
            seqs: seqs.into_iter().filter(|x| *x <= seq_end).map(u).collect(),
            updated: seq_end != row_id,
            timestamp: Timestamp(r.get(3)),
            turn_id: r.get(4),
            item,
        });
    }
    Ok(out)
}

/// The normalised render of each of `ids`.
///
/// # Errors
/// A statement error, a row that does not parse.
pub async fn parity(imp: &Importer, ids: &[Uuid]) -> Result<Vec<Render>> {
    // image URLs are not compared, so any key signs them
    let signer = pigeon::blobs::Signer::new(&pigeon::KeyList(vec![[0u8; 32]]));
    let ctx = ProjectCtx {
        zh: true,
        signer: &signer,
    };
    let mut out = Vec::new();
    for id in ids {
        let rows = rows(imp, *id)
            .await?
            .iter()
            .filter_map(|r| PaseoProjector.project(r, &ctx))
            .filter_map(|p| p.get("item").map(normalise))
            .collect();
        out.push(Render {
            conversation: *id,
            rows,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_normalise_to_their_six_fields() {
        let user = serde_json::json!({"type": "user_message", "text": "hi",
            "attachments": [{"fileName": "a.pdf"}], "images": [{"url": "x"}]});
        assert_eq!(
            normalise(&user),
            serde_json::json!([
                "user_message",
                "user",
                crate::sha_hex(b"hi"),
                null,
                null,
                ["a.pdf", "image 1"]
            ])
        );
        let tool = serde_json::json!({"type": "tool_call", "name": "Bash", "status": "completed",
            "metadata": {"sentFiles": [{"name": "r.pdf"}]}});
        assert_eq!(
            normalise(&tool),
            serde_json::json!(["tool_call", "assistant", null, "Bash", "completed", ["r.pdf"]])
        );
        let err = serde_json::json!({"type": "error", "message": "oops"});
        assert_eq!(normalise(&err)[2], crate::sha_hex(b"oops"));
    }
}

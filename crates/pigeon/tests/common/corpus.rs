//! The frame corpus of spec 18.1: one frame of every shape pigeon sends and of every shape of a client message pigeon
//! handles, committed at `testing/paseo-contract/fixtures/pigeon-frames.ndjson` and validated there against every
//! client build in `clients.txt` (server frames against the outbound schemas, client frames against the inbound
//! schema). Every socket test checks the frames its sockets saw: a shape the corpus lacks fails it, and its frames are
//! written to `target/tmp/pigeon-frames-missing/`, which `cargo test -p pigeon --test socket merge_frame_corpus --
//! --ignored` adds to the committed corpus, to be validated by `testing/paseo-contract` (`npm test`).
#![allow(dead_code, clippy::unwrap_used, clippy::missing_panics_doc)] // tests

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde_json::{Value, json};
use uuid::Uuid;

/// The committed corpus.
pub const CORPUS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../testing/paseo-contract/fixtures/pigeon-frames.ndjson"
);

/// Where a test writes the frames whose shapes the corpus lacks.
pub const MISSING: &str = concat!(env!("CARGO_TARGET_TMPDIR"), "/pigeon-frames-missing");

fn enum_like(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b == b'_' || b == b'.')
}

fn shape(v: &Value, path: &str, out: &mut BTreeSet<String>) {
    match v {
        Value::Object(o) => {
            out.insert(format!("{path}:object"));
            for (k, x) in o {
                shape(x, &format!("{path}.{k}"), out);
            }
        }
        Value::Array(a) => {
            out.insert(format!("{path}:array"));
            for x in a {
                shape(x, &format!("{path}[]"), out);
            }
        }
        Value::String(s) if enum_like(s) => {
            out.insert(format!("{path}={s}"));
        }
        Value::String(_) => {
            out.insert(format!("{path}:string"));
        }
        Value::Number(_) => {
            out.insert(format!("{path}:number"));
        }
        Value::Bool(_) => {
            out.insert(format!("{path}:bool"));
        }
        Value::Null => {
            out.insert(format!("{path}:null"));
        }
    }
}

/// A frame's shape: its direction, keys, value types and enum-like values (not ids, times or text).
pub fn signature(dir: &str, v: &Value) -> String {
    let mut s = BTreeSet::new();
    shape(v, "", &mut s);
    format!("{dir} {}", s.into_iter().collect::<Vec<_>>().join(" "))
}

/// Ids and times replaced by fixed values and long texts cut, so the committed corpus does not churn or grow.
pub fn normalise(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.iter().map(|(k, x)| (k.clone(), normalise(x))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(normalise).collect()),
        Value::String(s) if Uuid::parse_str(s).is_ok() => json!("00000000-0000-4000-8000-000000000000"),
        Value::String(s) if s.starts_with("wks_") && Uuid::parse_str(&s[4..]).is_ok() => {
            json!("wks_00000000-0000-4000-8000-000000000000")
        }
        Value::String(s) if magpie_api::Timestamp::parse(s).is_ok() => json!("2026-11-02T00:00:00.000Z"),
        Value::String(s) if s.contains("/api/blobs/") => json!("/api/blobs/b?v=display&sig=s"),
        Value::String(s) if s.len() > 2048 => {
            let mut end = 2048;
            while !s.is_char_boundary(end) {
                end -= 1;
            }
            json!(s[..end])
        }
        other => other.clone(),
    }
}

/// The committed corpus's signatures.
pub fn known() -> &'static BTreeSet<String> {
    static KNOWN: OnceLock<BTreeSet<String>> = OnceLock::new();
    KNOWN.get_or_init(|| {
        std::fs::read_to_string(CORPUS)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .map(|v| signature(v["dir"].as_str().unwrap_or("out"), &v["frame"]))
            .collect()
    })
}

/// One corpus line per shape of `records` ((case, dir, frame)).
pub fn lines(records: &[(String, &'static str, Value)]) -> BTreeMap<String, Value> {
    let mut seen = BTreeMap::new();
    for (case, dir, frame) in records {
        let n = normalise(frame);
        seen.entry(signature(dir, &n))
            .or_insert_with(|| json!({"case": case, "dir": dir, "frame": n}));
    }
    seen
}

/// Fails when a shape of `records` is not in the committed corpus; the missing lines go to [`MISSING`].
pub fn check(records: &[(String, &'static str, Value)]) {
    let missing: Vec<Value> = lines(records)
        .into_iter()
        .filter(|(s, _)| !known().contains(s))
        .map(|(_, v)| v)
        .collect();
    if missing.is_empty() {
        return;
    }
    std::fs::create_dir_all(MISSING).unwrap();
    let file = format!("{MISSING}/{}-{}.ndjson", std::process::id(), Uuid::new_v4());
    let mut out = String::new();
    for v in &missing {
        out.push_str(&v.to_string());
        out.push('\n');
    }
    std::fs::write(&file, out).unwrap();
    panic!(
        "{} frame shapes the corpus lacks (in {file}): add them to {CORPUS} (cargo test -p pigeon --test socket \
         merge_frame_corpus -- --ignored) and validate it in testing/paseo-contract: {missing:#?}",
        missing.len()
    );
}

/// Adds the frames of [`MISSING`] to the committed corpus (one line per shape, the corpus sorted by shape) and
/// empties [`MISSING`]; returns how many lines were added.
pub fn merge_missing() -> usize {
    let mut all: BTreeMap<String, Value> = BTreeMap::new();
    let mut add = |text: &str| {
        let mut n = 0;
        for v in text.lines().filter_map(|l| serde_json::from_str::<Value>(l).ok()) {
            let sig = signature(v["dir"].as_str().unwrap_or("out"), &v["frame"]);
            if let std::collections::btree_map::Entry::Vacant(e) = all.entry(sig) {
                e.insert(v);
                n += 1;
            }
        }
        n
    };
    add(&std::fs::read_to_string(CORPUS).unwrap_or_default());
    let mut added = 0;
    let mut files: Vec<_> = std::fs::read_dir(MISSING)
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.sort();
    for f in &files {
        added += add(&std::fs::read_to_string(f).unwrap());
        std::fs::remove_file(f).unwrap();
    }
    let mut out = String::new();
    for v in all.values() {
        out.push_str(&v.to_string());
        out.push('\n');
    }
    std::fs::write(CORPUS, out).unwrap();
    added
}

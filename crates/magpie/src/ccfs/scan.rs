//! One pass over a JSONL line (magpie 6.4): validates it as JSON and yields every string token with the byte range
//! of its content (without the quotes, still escaped) and its key path. The extraction rules (`extract.rs`) decide
//! from that what becomes a blob; the bytes are never re-encoded, so a stub plus the blobs reconstructs the line
//! exactly.
//!
//! The nesting limit is `serde_json`'s (128), so a line this scanner accepts is one the classifier can parse.

use std::ops::Range;

/// The deepest nesting accepted (`serde_json`'s default recursion limit).
pub const MAX_DEPTH: usize = 128;

/// One step of a key path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg<'a> {
    /// An object key, as written (escaped bytes, without quotes).
    Key(&'a [u8]),
    /// An array index.
    Index(usize),
}

/// Scans `line` (a JSONL line, its final `\n` included or not). Calls `f(path, content)` for every string value
/// (not for keys) in document order. `false` when the line is not one JSON value (invalid UTF-8 included); `f` may
/// have been called for a prefix of it.
pub fn scan<'a>(line: &'a [u8], f: impl FnMut(&[Seg<'a>], Range<usize>)) -> bool {
    if std::str::from_utf8(line).is_err() {
        return false;
    }
    let mut s = Scanner {
        b: line,
        i: 0,
        path: Vec::new(),
        f,
    };
    s.ws();
    if s.value(0).is_none() {
        return false;
    }
    s.ws();
    s.i == s.b.len()
}

struct Scanner<'a, F> {
    b: &'a [u8],
    i: usize,
    path: Vec<Seg<'a>>,
    f: F,
}

impl<'a, F: FnMut(&[Seg<'a>], Range<usize>)> Scanner<'a, F> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.peek() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        (self.peek()? == c).then(|| self.i += 1)
    }

    fn value(&mut self, depth: usize) -> Option<()> {
        match self.peek()? {
            b'{' => self.object(depth + 1),
            b'[' => self.array(depth + 1),
            b'"' => {
                let r = self.string()?;
                (self.f)(&self.path, r);
                Some(())
            }
            b't' => self.literal(b"true"),
            b'f' => self.literal(b"false"),
            b'n' => self.literal(b"null"),
            b'-' | b'0'..=b'9' => self.number(),
            _ => None,
        }
    }

    fn object(&mut self, depth: usize) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.eat(b'{')?;
        self.ws();
        if self.eat(b'}').is_some() {
            return Some(());
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            self.eat(b':')?;
            self.ws();
            self.path.push(Seg::Key(&self.b[key]));
            self.value(depth)?;
            self.path.pop();
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    fn array(&mut self, depth: usize) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.eat(b'[')?;
        self.ws();
        if self.eat(b']').is_some() {
            return Some(());
        }
        let mut n = 0;
        loop {
            self.ws();
            self.path.push(Seg::Index(n));
            self.value(depth)?;
            self.path.pop();
            n += 1;
            self.ws();
            match self.peek()? {
                b',' => self.i += 1,
                b']' => {
                    self.i += 1;
                    return Some(());
                }
                _ => return None,
            }
        }
    }

    /// A string at `i`; the range of its content.
    fn string(&mut self) -> Option<Range<usize>> {
        self.eat(b'"')?;
        let start = self.i;
        loop {
            let c = self.peek()?;
            match c {
                b'"' => {
                    let end = self.i;
                    self.i += 1;
                    return Some(start..end);
                }
                b'\\' => {
                    self.i += 1;
                    match self.peek()? {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => self.i += 1,
                        b'u' => {
                            self.i += 1;
                            for _ in 0..4 {
                                if !self.peek()?.is_ascii_hexdigit() {
                                    return None;
                                }
                                self.i += 1;
                            }
                        }
                        _ => return None,
                    }
                }
                0..=0x1f => return None,
                _ => self.i += 1,
            }
        }
    }

    fn literal(&mut self, lit: &[u8]) -> Option<()> {
        let end = self.i.checked_add(lit.len())?;
        (self.b.get(self.i..end)? == lit).then(|| self.i = end)
    }

    fn digits(&mut self) -> usize {
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self) -> Option<()> {
        let _ = self.eat(b'-');
        match self.peek()? {
            b'0' => self.i += 1,
            b'1'..=b'9' => {
                self.digits();
            }
            _ => return None,
        }
        if self.eat(b'.').is_some() && self.digits() == 0 {
            return None;
        }
        if let Some(b'e' | b'E') = self.peek() {
            self.i += 1;
            if let Some(b'+' | b'-') = self.peek() {
                self.i += 1;
            }
            if self.digits() == 0 {
                return None;
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use proptest::prelude::*;

    use super::*;

    fn strings(line: &str) -> Option<Vec<(String, String)>> {
        let mut out = Vec::new();
        let ok = scan(line.as_bytes(), |path, r| {
            let p: Vec<String> = path
                .iter()
                .map(|s| match s {
                    Seg::Key(k) => String::from_utf8_lossy(k).into_owned(),
                    Seg::Index(i) => i.to_string(),
                })
                .collect();
            out.push((p.join("."), line[r].to_owned()));
        });
        ok.then_some(out)
    }

    #[test]
    fn yields_every_string_value_with_its_path() {
        let got =
            strings(r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi \"x\""}]},"n":[1,"a"]}"#)
                .unwrap();
        assert_eq!(
            got,
            vec![
                ("type".into(), "assistant".into()),
                ("message.content.0.type".into(), "text".into()),
                ("message.content.0.text".into(), r#"hi \"x\""#.into()),
                ("n.1".into(), "a".into()),
            ]
        );
    }

    #[test]
    fn refuses_what_is_not_one_json_value() {
        for bad in [
            "",
            "{",
            "{\"a\":}",
            "[1,]",
            "{\"a\":1}x",
            "\"\\x\"",
            "01",
            "1.",
            "1e",
            "tru",
            "{\"a\" 1}",
            "\"a\nb\"",
            "{} {}",
        ] {
            assert!(strings(bad).is_none(), "{bad:?}");
        }
        assert!(!scan(b"\"\xff\"", |_, _| {}));
        let deep = format!("{}{}", "[".repeat(MAX_DEPTH + 1), "]".repeat(MAX_DEPTH + 1));
        assert!(strings(&deep).is_none());
        let ok = format!("{}{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(strings(&ok).is_some());
    }

    #[test]
    fn accepts_what_json_accepts() {
        for good in [
            "{}\n",
            " [ ] ",
            "-0.5e+10",
            "\"\\ud83d\\ude00\"",
            "null",
            "{\"a\":[true,false,null,{}]}\r\n",
        ] {
            assert!(strings(good).is_some(), "{good:?}");
        }
    }

    proptest! {
        /// The scanner agrees with serde_json on validity and yields exactly serde_json's string values.
        #[test]
        fn agrees_with_serde_json(v in json_value()) {
            let text = serde_json::to_string(&v).unwrap();
            let mut ours = Vec::new();
            let unescape = |r: Range<usize>| serde_json::from_str::<String>(&["\"", &text[r], "\""].concat()).unwrap();
            let ok = scan(text.as_bytes(), |_, r| ours.push(unescape(r)));
            prop_assert!(ok);
            let mut theirs = Vec::new();
            collect(&v, &mut theirs);
            prop_assert_eq!(ours, theirs);
        }

        #[test]
        fn never_panics_on_bytes(b in proptest::collection::vec(any::<u8>(), 0..200)) {
            // serde_json refuses a few values the grammar allows (a float out of range): those lines are stored as
            // internal lines either way
            let ok = scan(&b, |_, _| {});
            if serde_json::from_slice::<serde_json::Value>(&b).is_ok() {
                prop_assert!(ok);
            }
        }
    }

    fn collect(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(a) => a.iter().for_each(|x| collect(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| collect(x, out)),
            _ => {}
        }
    }

    pub(crate) fn json_value() -> impl Strategy<Value = serde_json::Value> {
        let leaf = prop_oneof![
            Just(serde_json::Value::Null),
            any::<bool>().prop_map(serde_json::Value::Bool),
            any::<i32>().prop_map(|n| serde_json::json!(n)),
            ".*".prop_map(serde_json::Value::String),
        ];
        leaf.prop_recursive(4, 32, 6, |inner| {
            prop_oneof![
                proptest::collection::vec(inner.clone(), 0..6).prop_map(serde_json::Value::Array),
                proptest::collection::btree_map(".{0,6}", inner, 0..6)
                    .prop_map(|m| serde_json::Value::Object(m.into_iter().collect())),
            ]
        })
    }
}

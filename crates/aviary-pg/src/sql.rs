//! A small SQL lexer: enough to split a script into statements and to see each statement's leading keywords, with
//! `--` and nested `/* */` comments, `'…'` / `E'…'` strings, `"…"` identifiers and `$tag$…$tag$` bodies honoured.

/// The statements of `sql`: comments removed, split at top-level `;`, trimmed, empty ones dropped.
pub(crate) fn statements(sql: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let b = sql.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                // to the end of the line; the newline itself separates tokens
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                cur.push(' ');
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let mut depth = 0usize;
                while i < b.len() {
                    if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                cur.push(' ');
            }
            b'\'' | b'"' => {
                // E'…' strings take backslash escapes; '' and "" are the quote itself either way
                let escapes = c == b'\'' && i > 0 && matches!(b[i - 1], b'e' | b'E') && !ident_byte_before(b, i - 1);
                let start = i;
                i += 1;
                while i < b.len() {
                    if escapes && b[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if b[i] == c {
                        if b.get(i + 1) == Some(&c) {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                cur.push_str(&sql[start..i.min(b.len())]);
            }
            b'$' if !ident_byte_before(b, i) => {
                if let Some(tag_end) = dollar_tag(b, i) {
                    let tag = &sql[i..=tag_end];
                    let body = tag_end + 1;
                    let end = sql[body..].find(tag).map_or(b.len(), |p| body + p + tag.len());
                    cur.push_str(&sql[i..end]);
                    i = end;
                } else {
                    cur.push('$');
                    i += 1;
                }
            }
            b';' => {
                push(&mut out, &mut cur);
                i += 1;
            }
            _ => {
                // copy one whole UTF-8 character
                let len = utf8_len(c);
                cur.push_str(&sql[i..(i + len).min(b.len())]);
                i += len;
            }
        }
    }
    push(&mut out, &mut cur);
    out
}

/// The first `n` words of one statement (comments already removed by [`statements`] or absent), lowercased.
pub(crate) fn leading_words(statement: &str, n: usize) -> Vec<String> {
    statement
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .take(n)
        .map(str::to_ascii_lowercase)
        .collect()
}

/// Whether a statement controls the transaction: `begin`, `start transaction`, `commit`, `end`, `abort`,
/// `rollback` (but not `rollback to [savepoint]`), `prepare transaction`, `commit/rollback prepared`; with
/// `savepoints = false` also `savepoint`, `release` and `rollback to`.
pub(crate) fn controls_transaction(statement: &str, savepoints: bool) -> bool {
    let w = leading_words(statement, 3);
    let w: Vec<&str> = w.iter().map(String::as_str).collect();
    match w.as_slice() {
        ["begin" | "commit" | "end" | "abort", ..] | ["start" | "prepare", "transaction", ..] => true,
        ["rollback", rest @ ..] => {
            let to = matches!(rest, ["to", ..] | ["transaction" | "work", "to", ..]);
            !to || !savepoints
        }
        ["savepoint" | "release", ..] => !savepoints,
        _ => false,
    }
}

fn push(out: &mut Vec<String>, cur: &mut String) {
    let s = cur.trim();
    if !s.is_empty() {
        out.push(s.to_owned());
    }
    cur.clear();
}

fn ident_byte_before(b: &[u8], i: usize) -> bool {
    i > 0 && (b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'$')
}

/// The index of the closing `$` of a dollar-quote tag opening at `i` (`$$` or `$tag$`), if one opens there.
fn dollar_tag(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'$' => return Some(j),
            c if c.is_ascii_alphabetic() || c == b'_' || (j > i + 1 && c.is_ascii_digit()) || c >= 0x80 => j += 1,
            _ => return None,
        }
    }
    None
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_at_top_level_semicolons_only() {
        assert_eq!(statements("select 1; select 2;"), ["select 1", "select 2"]);
        assert_eq!(statements("select ';'"), ["select ';'"]);
        assert_eq!(statements("select 'it''s; fine'"), ["select 'it''s; fine'"]);
        assert_eq!(statements(r"select E'a\'; b'"), [r"select E'a\'; b'"]);
        assert_eq!(statements("select \"a;b\" from t"), ["select \"a;b\" from t"]);
        assert_eq!(
            statements("create function f() returns int as $$ select 1; $$ language sql; select 2"),
            [
                "create function f() returns int as $$ select 1; $$ language sql",
                "select 2"
            ]
        );
        assert_eq!(
            statements("do $body$ begin perform 1; end $body$"),
            ["do $body$ begin perform 1; end $body$"]
        );
        // $1 is a parameter, not a tag
        assert_eq!(statements("select $1; select $2"), ["select $1", "select $2"]);
    }

    #[test]
    fn comments_are_removed_wherever_they_are() {
        assert_eq!(
            statements("create index concurrently i on t (x); -- one; two\n"),
            ["create index concurrently i on t (x)"]
        );
        assert_eq!(
            statements("/* a; b */ create index concurrently i on t (x) /* c; /* nested; */ d */;"),
            ["create index concurrently i on t (x)"]
        );
        assert_eq!(statements("-- only\n/* comments; */\n;;"), Vec::<String>::new());
        assert_eq!(statements("select 1 -- x\n; select 2"), ["select 1", "select 2"]);
        // a comment marker inside a string is text
        assert_eq!(
            statements("select '--;' ; select '/*'"),
            ["select '--;'", "select '/*'"]
        );
        assert_eq!(statements("select 1 /* unterminated; "), ["select 1"]);
        assert_eq!(statements("select '中文;' ; select 2"), ["select '中文;'", "select 2"]);
    }

    #[test]
    fn transaction_control_is_recognised() {
        for s in [
            "begin",
            "BEGIN ISOLATION LEVEL SERIALIZABLE",
            "start transaction",
            "commit",
            "end",
            "abort",
            "rollback",
            "rollback work",
            "prepare transaction 'x'",
            "commit prepared 'x'",
            "rollback prepared 'x'",
        ] {
            assert!(controls_transaction(s, true), "{s}");
            assert!(controls_transaction(s, false), "{s}");
        }
        for s in [
            "savepoint a",
            "release a",
            "release savepoint a",
            "rollback to a",
            "rollback transaction to savepoint a",
        ] {
            assert!(!controls_transaction(s, true), "{s}");
            assert!(controls_transaction(s, false), "{s}");
        }
        for s in [
            "select 1",
            "insert into begins values (1)",
            "update t set commit = 1",
            "prepare p as select 1",
        ] {
            assert!(!controls_transaction(s, false), "{s}");
        }
    }
}

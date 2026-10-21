//! JSON without a heap: a validating scanner over a byte slice (no recursion: nesting is tracked in a 64-bit
//! stack, deeper input is refused) and a writer into a fixed buffer.
use crate::fixed::Buf;
use core::fmt::Write as _;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct JsonError;

pub type R<T> = Result<T, JsonError>;

/// Nesting limit for any JSON sandboxd reads.
pub const MAX_DEPTH: u32 = 64;

/// A JSON string as it appears between the quotes (escapes not yet decoded, already validated).
#[derive(Clone, Copy, Debug)]
pub struct JStr<'a>(pub &'a [u8]);

impl<'a> JStr<'a> {
    pub fn has_escapes(&self) -> bool {
        self.0.contains(&b'\\')
    }
    /// The decoded string, in `out` (None if it doesn't fit).
    pub fn decode<'b>(&self, out: &'b mut [u8]) -> Option<&'b str> {
        let s = self.0;
        let mut o = 0usize;
        let mut i = 0usize;
        let mut put = |o: &mut usize, bytes: &[u8]| -> bool {
            if *o + bytes.len() > out.len() {
                return false;
            }
            out[*o..*o + bytes.len()].copy_from_slice(bytes);
            *o += bytes.len();
            true
        };
        while i < s.len() {
            let c = s[i];
            if c != b'\\' {
                // copy the run up to the next escape
                let start = i;
                while i < s.len() && s[i] != b'\\' {
                    i += 1;
                }
                if !put(&mut o, &s[start..i]) {
                    return None;
                }
                continue;
            }
            let e = s[i + 1];
            i += 2;
            let ch: char = match e {
                b'"' => '"',
                b'\\' => '\\',
                b'/' => '/',
                b'b' => '\u{8}',
                b'f' => '\u{c}',
                b'n' => '\n',
                b'r' => '\r',
                b't' => '\t',
                b'u' => {
                    let hi = hex4(&s[i..i + 4]);
                    i += 4;
                    if (0xD800..0xDC00).contains(&hi) && i + 6 <= s.len() && s[i] == b'\\' && s[i + 1] == b'u' {
                        let lo = hex4(&s[i + 2..i + 6]);
                        if (0xDC00..0xE000).contains(&lo) {
                            i += 6;
                            char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{fffd}')
                        } else {
                            '\u{fffd}'
                        }
                    } else {
                        char::from_u32(hi).unwrap_or('\u{fffd}')
                    }
                }
                _ => return None, // validated before; unreachable
            };
            let mut tmp = [0u8; 4];
            if !put(&mut o, ch.encode_utf8(&mut tmp).as_bytes()) {
                return None;
            }
        }
        core::str::from_utf8(&out[..o]).ok()
    }
    /// Equal to `k` once decoded.
    pub fn is(&self, k: &str) -> bool {
        if !self.has_escapes() {
            return self.0 == k.as_bytes();
        }
        let mut tmp = [0u8; 256];
        self.decode(&mut tmp) == Some(k)
    }
}

fn hex4(b: &[u8]) -> u32 {
    let mut v = 0u32;
    for &c in b {
        v = v * 16 + (c as char).to_digit(16).unwrap_or(0);
    }
    v
}

/// A value. Objects and arrays are given as their raw text (parse them with `object` / `array`).
#[derive(Clone, Copy, Debug)]
pub enum Val<'a> {
    Null,
    Bool(bool),
    Num(f64),
    Str(JStr<'a>),
    Obj(&'a [u8]),
    Arr(&'a [u8]),
}

impl<'a> Val<'a> {
    pub fn str(&self) -> Option<JStr<'a>> {
        if let Val::Str(s) = self { Some(*s) } else { None }
    }
    pub fn num(&self) -> Option<f64> {
        if let Val::Num(n) = self { Some(*n) } else { None }
    }
}

struct Lex<'a> {
    s: &'a [u8],
    i: usize,
}

impl<'a> Lex<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn eat(&mut self, c: u8) -> R<()> {
        self.ws();
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            Err(JsonError)
        }
    }
    /// At a '"': scans the string, returns its contents.
    fn string(&mut self) -> R<JStr<'a>> {
        if self.peek() != Some(b'"') {
            return Err(JsonError);
        }
        self.i += 1;
        let start = self.i;
        loop {
            let c = *self.s.get(self.i).ok_or(JsonError)?;
            match c {
                b'"' => {
                    let r = JStr(&self.s[start..self.i]);
                    self.i += 1;
                    return Ok(r);
                }
                b'\\' => {
                    let e = *self.s.get(self.i + 1).ok_or(JsonError)?;
                    match e {
                        b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => self.i += 2,
                        b'u' => {
                            let h = self.s.get(self.i + 2..self.i + 6).ok_or(JsonError)?;
                            if !h.iter().all(|c| c.is_ascii_hexdigit()) {
                                return Err(JsonError);
                            }
                            self.i += 6;
                        }
                        _ => return Err(JsonError),
                    }
                }
                0..=0x1f => return Err(JsonError),
                _ => self.i += 1,
            }
        }
    }
    fn number(&mut self) -> R<f64> {
        let start = self.i;
        let s = self.s;
        let digits = |i: &mut usize| {
            let st = *i;
            while *i < s.len() && s[*i].is_ascii_digit() {
                *i += 1;
            }
            *i - st
        };
        let mut i = self.i;
        if s.get(i) == Some(&b'-') {
            i += 1;
        }
        if s.get(i) == Some(&b'0') {
            i += 1;
        } else if digits(&mut i) == 0 {
            return Err(JsonError);
        }
        if s.get(i) == Some(&b'.') {
            i += 1;
            if digits(&mut i) == 0 {
                return Err(JsonError);
            }
        }
        if matches!(s.get(i), Some(b'e' | b'E')) {
            i += 1;
            if matches!(s.get(i), Some(b'+' | b'-')) {
                i += 1;
            }
            if digits(&mut i) == 0 {
                return Err(JsonError);
            }
        }
        self.i = i;
        let t = core::str::from_utf8(&s[start..i]).map_err(|_| JsonError)?;
        t.parse::<f64>().map_err(|_| JsonError)
    }
    fn literal(&mut self, w: &[u8]) -> R<()> {
        if self.s.get(self.i..self.i + w.len()) == Some(w) {
            self.i += w.len();
            Ok(())
        } else {
            Err(JsonError)
        }
    }
    /// Skips one complete value starting at a '{' or '[' (validating it), without recursion.
    fn skip_container(&mut self) -> R<()> {
        let mut stack: u64 = 0; // bit d set = level d is an object
        let mut depth: u32 = 0;
        // state: expecting a value
        'value: loop {
            self.ws();
            match self.peek().ok_or(JsonError)? {
                c @ (b'{' | b'[') => {
                    if depth >= MAX_DEPTH {
                        return Err(JsonError);
                    }
                    self.i += 1;
                    let obj = c == b'{';
                    if obj {
                        stack |= 1 << depth;
                    } else {
                        stack &= !(1 << depth);
                    }
                    depth += 1;
                    self.ws();
                    let close = if obj { b'}' } else { b']' };
                    if self.peek() == Some(close) {
                        self.i += 1;
                        depth -= 1;
                    } else {
                        if obj {
                            self.string()?;
                            self.eat(b':')?;
                        }
                        continue 'value;
                    }
                }
                b'"' => {
                    self.string()?;
                }
                b't' => self.literal(b"true")?,
                b'f' => self.literal(b"false")?,
                b'n' => self.literal(b"null")?,
                b'-' | b'0'..=b'9' => {
                    self.number()?;
                }
                _ => return Err(JsonError),
            }
            // after a value
            loop {
                if depth == 0 {
                    return Ok(());
                }
                self.ws();
                let obj = stack & (1 << (depth - 1)) != 0;
                match self.peek().ok_or(JsonError)? {
                    b',' => {
                        self.i += 1;
                        if obj {
                            self.ws();
                            self.string()?;
                            self.eat(b':')?;
                        }
                        continue 'value;
                    }
                    b'}' if obj => {
                        self.i += 1;
                        depth -= 1;
                    }
                    b']' if !obj => {
                        self.i += 1;
                        depth -= 1;
                    }
                    _ => return Err(JsonError),
                }
            }
        }
    }
    fn value(&mut self) -> R<Val<'a>> {
        self.ws();
        let start = self.i;
        match self.peek().ok_or(JsonError)? {
            b'{' => {
                self.skip_container()?;
                Ok(Val::Obj(&self.s[start..self.i]))
            }
            b'[' => {
                self.skip_container()?;
                Ok(Val::Arr(&self.s[start..self.i]))
            }
            b'"' => Ok(Val::Str(self.string()?)),
            b't' => self.literal(b"true").map(|_| Val::Bool(true)),
            b'f' => self.literal(b"false").map(|_| Val::Bool(false)),
            b'n' => self.literal(b"null").map(|_| Val::Null),
            b'-' | b'0'..=b'9' => Ok(Val::Num(self.number()?)),
            _ => Err(JsonError),
        }
    }
}

/// The single value that is the whole of `s` (whitespace around it allowed). Validates all of it.
pub fn parse(s: &[u8]) -> R<Val<'_>> {
    if core::str::from_utf8(s).is_err() {
        return Err(JsonError);
    }
    let mut l = Lex { s, i: 0 };
    let v = l.value()?;
    l.ws();
    if l.i != s.len() {
        return Err(JsonError);
    }
    Ok(v)
}

/// Calls `f` for each member of the object `s` (raw text from `Val::Obj`, or a document; validated).
pub fn object<'a, E: From<JsonError>>(
    s: &'a [u8],
    mut f: impl FnMut(JStr<'a>, Val<'a>) -> Result<(), E>,
) -> Result<(), E> {
    let mut l = Lex { s, i: 0 };
    l.eat(b'{')?;
    l.ws();
    if l.peek() == Some(b'}') {
        l.i += 1;
    } else {
        loop {
            l.ws();
            let k = l.string()?;
            l.eat(b':')?;
            let v = l.value()?;
            f(k, v)?;
            l.ws();
            match l.peek() {
                Some(b',') => l.i += 1,
                Some(b'}') => {
                    l.i += 1;
                    break;
                }
                _ => return Err(JsonError.into()),
            }
        }
    }
    l.ws();
    if l.i != s.len() {
        return Err(JsonError.into());
    }
    Ok(())
}

/// Calls `f` for each element of the array `s` (raw text from `Val::Arr`).
pub fn array<'a, E: From<JsonError>>(s: &'a [u8], mut f: impl FnMut(Val<'a>) -> Result<(), E>) -> Result<(), E> {
    let mut l = Lex { s, i: 0 };
    l.eat(b'[')?;
    l.ws();
    if l.peek() == Some(b']') {
        l.i += 1;
    } else {
        loop {
            let v = l.value()?;
            f(v)?;
            l.ws();
            match l.peek() {
                Some(b',') => l.i += 1,
                Some(b']') => {
                    l.i += 1;
                    break;
                }
                _ => return Err(JsonError.into()),
            }
        }
    }
    l.ws();
    if l.i != s.len() {
        return Err(JsonError.into());
    }
    Ok(())
}

// ---- writing ----------------------------------------------------------------------------------------------------

/// Writes `s` as a JSON string (quoted, escaped).
pub fn write_str(b: &mut Buf, s: &str) {
    b.s("\"");
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &c) in bytes.iter().enumerate() {
        let esc: Option<&str> = match c {
            b'"' => Some("\\\""),
            b'\\' => Some("\\\\"),
            b'\n' => Some("\\n"),
            b'\r' => Some("\\r"),
            b'\t' => Some("\\t"),
            0..=0x1f | 0x7f => None,
            _ => continue,
        };
        b.bytes(&bytes[start..i]);
        match esc {
            Some(e) => b.s(e),
            None => {
                let _ = write!(b, "\\u{:04x}", c);
            }
        }
        start = i + 1;
    }
    b.bytes(&bytes[start..]);
    b.s("\"");
}

/// A JSON writer with comma bookkeeping (nesting up to 16; deeper marks the buffer full).
pub struct W<'x, 'a> {
    pub b: &'x mut Buf<'a>,
    need: u16,
    depth: u8,
    after_key: bool,
}

impl<'x, 'a> W<'x, 'a> {
    pub fn new(b: &'x mut Buf<'a>) -> Self {
        W { b, need: 0, depth: 0, after_key: false }
    }
    fn pre(&mut self) {
        if self.after_key {
            self.after_key = false;
            return;
        }
        let bit = 1u16 << self.depth;
        if self.need & bit != 0 {
            self.b.s(",");
        }
        self.need |= bit;
    }
    fn open(&mut self, c: &str) {
        self.pre();
        self.b.s(c);
        if self.depth >= 15 {
            self.b.full = true;
            return;
        }
        self.depth += 1;
        self.need &= !(1 << self.depth);
    }
    fn close(&mut self, c: &str) {
        self.b.s(c);
        self.depth = self.depth.saturating_sub(1);
    }
    /// Continue inside an object whose `{` and at least one member are already in the buffer.
    pub fn obj_continue(&mut self) -> &mut Self {
        self.depth = 1;
        self.need = 1 << 1;
        self
    }
    pub fn obj(&mut self) -> &mut Self {
        self.open("{");
        self
    }
    pub fn end(&mut self) -> &mut Self {
        self.close("}");
        self
    }
    pub fn arr(&mut self) -> &mut Self {
        self.open("[");
        self
    }
    pub fn end_arr(&mut self) -> &mut Self {
        self.close("]");
        self
    }
    pub fn key(&mut self, k: &str) -> &mut Self {
        self.pre();
        write_str(self.b, k);
        self.b.s(":");
        self.after_key = true;
        self
    }
    pub fn str(&mut self, s: &str) -> &mut Self {
        self.pre();
        write_str(self.b, s);
        self
    }
    /// One string made of `parts` concatenated.
    pub fn str_parts(&mut self, parts: &[&str]) -> &mut Self {
        self.pre();
        self.b.s("\"");
        for p in parts {
            let mark = self.b.len;
            write_str(self.b, p);
            // drop the quotes write_str added around this part
            if !self.b.full {
                let end = self.b.len;
                self.b.b.copy_within(mark + 1..end - 1, mark);
                self.b.len = end - 2;
            }
        }
        self.b.s("\"");
        self
    }
    /// The string "k=v".
    pub fn pre_str_kv(&mut self, k: &str, v: &str) -> &mut Self {
        self.str_parts(&[k, "=", v])
    }
    pub fn u64(&mut self, n: u64) -> &mut Self {
        self.pre();
        let _ = write!(self.b, "{n}");
        self
    }
    /// A number as JavaScript prints it for the values sandboxd sends (integers without a fraction).
    pub fn f64(&mut self, n: f64) -> &mut Self {
        self.pre();
        if !n.is_finite() {
            self.b.s("null");
        } else if n == n.trunc() && n.abs() < 1e15 {
            let _ = write!(self.b, "{}", n as i64);
        } else {
            let _ = write!(self.b, "{n}");
        }
        self
    }
    pub fn bool(&mut self, v: bool) -> &mut Self {
        self.pre();
        self.b.s(if v { "true" } else { "false" });
        self
    }
    pub fn null(&mut self) -> &mut Self {
        self.pre();
        self.b.s("null");
        self
    }
    /// Already valid JSON (e.g. a stored stats object).
    pub fn raw(&mut self, j: &[u8]) -> &mut Self {
        self.pre();
        self.b.bytes(j);
        self
    }
    pub fn kstr(&mut self, k: &str, v: &str) -> &mut Self {
        self.key(k).str(v)
    }
    pub fn ku64(&mut self, k: &str, v: u64) -> &mut Self {
        self.key(k).u64(v)
    }
    pub fn kf64(&mut self, k: &str, v: f64) -> &mut Self {
        self.key(k).f64(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_rejects() {
        let doc = br#" {"a": [1, 2.5e3, -0, {"x": "y\n\u00e9\ud83d\ude00"}], "b": true, "c": null, "d": "q\"q"} "#;
        let mut seen = 0;
        object::<JsonError>(parse(doc).map(|v| if let Val::Obj(o) = v { o } else { b"" }).unwrap(), |k, v| {
            seen += 1;
            if k.is("a") {
                let mut n = 0;
                array::<JsonError>(if let Val::Arr(a) = v { a } else { b"" }, |e| {
                    if let Val::Obj(o) = e {
                        object::<JsonError>(o, |_, v| {
                            let mut out = [0u8; 32];
                            assert_eq!(v.str().unwrap().decode(&mut out), Some("y\n\u{e9}\u{1f600}"));
                            Ok(())
                        })?;
                    }
                    n += 1;
                    Ok(())
                })
                .unwrap();
                assert_eq!(n, 4);
            }
            if k.is("d") {
                let mut out = [0u8; 8];
                assert_eq!(v.str().unwrap().decode(&mut out), Some("q\"q"));
            }
            Ok(())
        })
        .unwrap();
        assert_eq!(seen, 4);
        for bad in [
            &b"{"[..],
            b"{\"a\":}",
            b"[1,]",
            b"{\"a\":1,}",
            b"01",
            b"\"\x01\"",
            b"nul",
            b"{} x",
            b"[\"\\x\"]",
            b"1.",
            b"-",
        ] {
            assert!(parse(bad).is_err(), "{:?}", core::str::from_utf8(bad));
        }
        // deep nesting is refused, without recursion
        let mut deep = [b'['; 70];
        deep[69] = b']';
        assert!(parse(&deep).is_err());
        let mut big = [0u8; 20000];
        for (i, c) in big.iter_mut().enumerate() {
            *c = if i < 10000 { b'[' } else { b']' };
        }
        assert!(parse(&big).is_err());
    }

    #[test]
    fn writes() {
        let mut mem = [0u8; 256];
        let mut b = Buf::new(&mut mem);
        let mut w = W::new(&mut b);
        w.obj()
            .kstr("a", "x\"\u{1}")
            .key("b")
            .arr()
            .u64(1)
            .f64(0.125)
            .f64(2.0)
            .null()
            .end_arr()
            .key("c")
            .obj()
            .end()
            .end();
        assert_eq!(b.as_str(), r#"{"a":"x\"\u0001","b":[1,0.125,2,null],"c":{}}"#);
        let mut small = [0u8; 4];
        let mut b = Buf::new(&mut small);
        W::new(&mut b).obj().kstr("long", "value").end();
        assert!(b.full);
    }
}

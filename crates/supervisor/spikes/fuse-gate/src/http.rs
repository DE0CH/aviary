//! A minimal HTTP/1.1 server and keep-alive client over std, enough for the fake magpie and the mock model.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn q(&self, k: &str) -> &str {
        self.query.get(k).map_or("", String::as_str)
    }
    pub fn qu64(&self, k: &str) -> u64 {
        self.q(k).parse().unwrap_or(0)
    }
}

pub fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn pct_encode(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-._~/".contains(&c) {
            o.push(c as char);
        } else {
            o.push_str(&format!("%{c:02X}"));
        }
    }
    o
}

fn parse_target(t: &str) -> (String, HashMap<String, String>) {
    let (p, q) = t.split_once('?').unwrap_or((t, ""));
    let mut m = HashMap::new();
    for kv in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        m.insert(pct_decode(k), pct_decode(v));
    }
    (p.to_string(), m)
}

/// What a handler answers: a whole body, or it takes the stream and writes the response itself.
pub enum Reply {
    Full(u16, &'static str, Vec<u8>),
    Streamed,
}

pub fn serve<F>(addr: &str, handler: F) -> io::Result<()>
where
    F: Fn(Request, &mut TcpStream) -> Reply + Send + Sync + 'static,
{
    let l = TcpListener::bind(addr)?;
    let h = Arc::new(handler);
    for s in l.incoming() {
        let Ok(s) = s else { continue };
        let h = h.clone();
        thread::spawn(move || {
            let _ = s.set_nodelay(true);
            let mut w = match s.try_clone() {
                Ok(w) => w,
                Err(_) => return,
            };
            let mut r = BufReader::new(s);
            loop {
                let Ok(Some(req)) = read_request(&mut r) else { return };
                match h(req, &mut w) {
                    Reply::Full(code, ct, body) => {
                        let head = format!(
                            "HTTP/1.1 {code} X\r\nContent-Type: {ct}\r\nContent-Length: {}\r\n\r\n",
                            body.len()
                        );
                        if w.write_all(head.as_bytes()).is_err() || w.write_all(&body).is_err() {
                            return;
                        }
                    }
                    Reply::Streamed => return,
                }
            }
        });
    }
    Ok(())
}

fn read_request(r: &mut BufReader<TcpStream>) -> io::Result<Option<Request>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    let mut it = line.split_whitespace();
    let method = it.next().unwrap_or("").to_string();
    let (path, query) = parse_target(it.next().unwrap_or("/"));
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if r.read_line(&mut h)? == 0 {
            return Ok(None);
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                len = v.trim().parse().unwrap_or(0);
            }
        }
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    Ok(Some(Request { method, path, query, body }))
}

/// A keep-alive client to one address; reconnects on any error (the caller retries).
pub struct Client {
    addr: String,
    conn: Option<(TcpStream, BufReader<TcpStream>)>,
}

impl Client {
    pub fn new(addr: &str) -> Self {
        Self { addr: addr.to_string(), conn: None }
    }

    /// Sends the request; `after_send` runs once the request bytes are written (fault injection).
    pub fn call(
        &mut self,
        method: &str,
        target: &str,
        body: &[u8],
        after_send: &mut dyn FnMut(),
    ) -> io::Result<(u16, Vec<u8>)> {
        if self.conn.is_none() {
            let s = TcpStream::connect(&self.addr)?;
            s.set_nodelay(true)?;
            let r = BufReader::new(s.try_clone()?);
            self.conn = Some((s, r));
        }
        let res = (|| {
            let (w, r) = self.conn.as_mut().ok_or_else(|| io::Error::other("no conn"))?;
            let head = format!("{method} {target} HTTP/1.1\r\nHost: m\r\nContent-Length: {}\r\n\r\n", body.len());
            w.write_all(head.as_bytes())?;
            w.write_all(body)?;
            after_send();
            let mut line = String::new();
            if r.read_line(&mut line)? == 0 {
                return Err(io::Error::other("closed"));
            }
            let code: u16 = line.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h)? == 0 {
                    return Err(io::Error::other("closed"));
                }
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.eq_ignore_ascii_case("content-length") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut b = vec![0; len];
            r.read_exact(&mut b)?;
            Ok((code, b))
        })();
        if res.is_err() {
            self.conn = None;
        }
        res
    }
}

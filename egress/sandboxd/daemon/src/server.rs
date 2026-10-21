//! TLS 1.3 with client certificates (rustls) and a minimal HTTP/1.1 (Content-Length bodies, keep-alive) over fixed
//! buffers. One connection per worker thread at a time; rustls' state lives on that worker's arena and is checked
//! against it before every call (`guard`), so a connection that would need more is closed, never the process.
use crate::alloc::{current_usage, in_tls};
use crate::fixed::{Buf, Str};
use core::fmt::Write as _;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::{NoServerSessionStorage, WebPkiClientVerifier};
use rustls::{RootCertStore, ServerConfig, ServerConnection};
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

/// The client certificate's CN sandboxd accepts (the controller's).
pub const CONTROLLER_CN: &str = "sandbox-controller";

/// Free arena space required before any call into rustls: the most one call can allocate (a full handshake message
/// of 64 KiB being joined, plus a record, plus the certificate chain's parse) with room to spare.
pub const TLS_HEADROOM: usize = 200 << 10;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IoErr {
    /// peer closed, timed out or broke the protocol: just close
    Closed,
    /// this connection would need more memory than its arena has: close it
    Budget,
}

pub type IoRes<T> = Result<T, IoErr>;

/// The server's TLS configuration (made once, at start, on the boot arena).
pub fn tls_config(cert: &str, key: &str, ca: &str) -> Result<Arc<ServerConfig>, Str<300>> {
    let e = |what: &str, x: &dyn core::fmt::Display| {
        let mut s: Str<300> = Str::new();
        let _ = write!(s, "{what}: {x}");
        s
    };
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut roots = RootCertStore::empty();
    for c in CertificateDer::pem_file_iter(ca).map_err(|x| e(ca, &x))? {
        roots.add(c.map_err(|x| e(ca, &x))?).map_err(|x| e(ca, &x))?;
    }
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider.clone())
        .build()
        .map_err(|x| e("client verifier", &x))?;
    let chain: Result<Vec<_>, _> = CertificateDer::pem_file_iter(cert).map_err(|x| e(cert, &x))?.collect();
    let chain = chain.map_err(|x| e(cert, &x))?;
    let key = PrivateKeyDer::from_pem_file(key).map_err(|x| e(key, &x))?;
    let mut cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|x| e("tls", &x))?
        .with_client_cert_verifier(verifier)
        .with_single_cert(chain, key)
        .map_err(|x| e("certificate", &x))?;
    // nothing that grows across connections: no session cache, no tickets
    cfg.session_storage = Arc::new(NoServerSessionStorage {});
    cfg.send_tls13_tickets = 0;
    Ok(Arc::new(cfg))
}

/// The subject CN of a DER certificate (a bounded walk, no recursion, no allocation).
pub fn subject_cn(der: &[u8]) -> Option<&str> {
    // TLV reader
    fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let tag = *b.first()?;
        let l0 = *b.get(1)? as usize;
        let (len, hdr) = if l0 < 0x80 {
            (l0, 2)
        } else {
            let n = l0 & 0x7f;
            if n == 0 || n > 3 {
                return None;
            }
            let mut len = 0usize;
            for i in 0..n {
                len = (len << 8) | *b.get(2 + i)? as usize;
            }
            (len, 2 + n)
        };
        let body = b.get(hdr..hdr + len)?;
        Some((tag, body, &b[hdr + len..]))
    }
    let (_, cert, _) = tlv(der)?; // Certificate SEQUENCE
    let (_, tbs, _) = tlv(cert)?; // TBSCertificate SEQUENCE
    let mut rest = tbs;
    // [0] version (optional), serial, signature alg, issuer, validity, subject
    let (tag, _, r) = tlv(rest)?;
    rest = if tag == 0xa0 { r } else { tbs };
    for _ in 0..4 {
        let (_, _, r) = tlv(rest)?;
        rest = r;
    }
    let (_, subject, _) = tlv(rest)?; // Name: SEQUENCE OF RDN (SET OF AttributeTypeAndValue)
    let mut rdns = subject;
    let mut found = None;
    while !rdns.is_empty() {
        let (_, set, r) = tlv(rdns)?;
        rdns = r;
        let mut atvs = set;
        while !atvs.is_empty() {
            let (_, atv, r) = tlv(atvs)?;
            atvs = r;
            let (_, oid, v) = tlv(atv)?;
            if oid == [0x55, 0x04, 0x03] {
                let (_, val, _) = tlv(v)?;
                found = core::str::from_utf8(val).ok();
            }
        }
    }
    found
}

/// Server side of one TLS connection on this worker's arena.
pub struct Tls {
    pub conn: ServerConnection,
    pub sock: TcpStream,
}

fn guard() -> IoRes<()> {
    let (used, size) = current_usage();
    if size.saturating_sub(used) < TLS_HEADROOM { Err(IoErr::Budget) } else { Ok(()) }
}

impl Tls {
    /// The handshake, which must finish by `deadline` (monotonic ms).
    pub fn accept(cfg: &Arc<ServerConfig>, sock: TcpStream, deadline: u64) -> IoRes<Tls> {
        guard()?;
        let conn = in_tls(|| ServerConnection::new(cfg.clone())).map_err(|_| IoErr::Closed)?;
        let mut t = Tls { conn, sock };
        in_tls(|| t.conn.set_buffer_limit(Some(64 << 10)));
        while t.conn.is_handshaking() {
            if crate::util::mono_ms() > deadline {
                in_tls(|| drop(t));
                return Err(IoErr::Closed);
            }
            t.flush()?;
            if !t.conn.is_handshaking() {
                break;
            }
            if !t.pull()? {
                return Err(IoErr::Closed);
            }
        }
        t.flush()?;
        Ok(t)
    }

    /// The client certificate's CN.
    pub fn peer_cn(&self) -> Option<Str<64>> {
        let certs = self.conn.peer_certificates()?;
        let first = certs.first()?;
        subject_cn(first.as_ref()).and_then(Str::from)
    }

    /// Reads TLS records from the socket and processes them. Ok(false) on EOF.
    fn pull(&mut self) -> IoRes<bool> {
        guard()?;
        let n = in_tls(|| self.conn.read_tls(&mut self.sock)).map_err(|_| IoErr::Closed)?;
        if n == 0 {
            return Ok(false);
        }
        guard()?;
        let r = in_tls(|| self.conn.process_new_packets().map(|_| ()));
        if r.is_err() {
            // send the alert, then close
            let _ = self.flush();
            return Err(IoErr::Closed);
        }
        Ok(true)
    }

    pub fn flush(&mut self) -> IoRes<()> {
        while self.conn.wants_write() {
            guard()?;
            in_tls(|| self.conn.write_tls(&mut self.sock)).map_err(|_| IoErr::Closed)?;
        }
        Ok(())
    }

    pub fn close(mut self) {
        in_tls(|| self.conn.send_close_notify());
        let _ = self.flush();
        in_tls(|| drop(self));
    }
}

/// What the HTTP layer reads from and writes to.
pub trait Stream {
    /// Ok(0) = closed
    fn read_some(&mut self, buf: &mut [u8]) -> IoRes<usize>;
    fn write_all_bytes(&mut self, b: &[u8]) -> IoRes<()>;
    fn sock(&self) -> &TcpStream;
}

impl Stream for Tls {
    fn read_some(&mut self, buf: &mut [u8]) -> IoRes<usize> {
        loop {
            match in_tls(|| self.conn.reader().read(buf)) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(IoErr::Closed),
            }
            self.flush()?;
            if !self.pull()? {
                return Ok(0);
            }
        }
    }
    fn write_all_bytes(&mut self, mut b: &[u8]) -> IoRes<()> {
        while !b.is_empty() {
            guard()?;
            let n = in_tls(|| self.conn.writer().write(&b[..b.len().min(16 << 10)])).map_err(|_| IoErr::Closed)?;
            if n == 0 {
                self.flush()?;
                continue;
            }
            b = &b[n..];
            self.flush()?;
        }
        Ok(())
    }
    fn sock(&self) -> &TcpStream {
        &self.sock
    }
}

impl Stream for TcpStream {
    fn read_some(&mut self, buf: &mut [u8]) -> IoRes<usize> {
        self.read(buf).map_err(|_| IoErr::Closed)
    }
    fn write_all_bytes(&mut self, b: &[u8]) -> IoRes<()> {
        self.write_all(b).map_err(|_| IoErr::Closed)
    }
    fn sock(&self) -> &TcpStream {
        self
    }
}

// ---- HTTP/1.1 -----------------------------------------------------------------------------------------------------

/// Idle time a kept-alive connection waits for its next request (Node's server: 5 s, announced as timeout=5).
pub const IDLE_MS: u64 = 6_000;
/// Time to receive a request's head and body (Node: headersTimeout 30 s).
pub const REQUEST_MS: u64 = 30_000;

pub struct Req<'a> {
    pub method: &'a str,
    pub path: &'a str,
    pub query: &'a str,
    pub body: &'a [u8],
    pub close: bool,
}

pub enum ReadErr {
    /// nothing more on this connection
    Closed,
    /// answer with this status, then close
    Status(u16, &'static str),
    Budget,
}

impl From<IoErr> for ReadErr {
    fn from(e: IoErr) -> Self {
        match e {
            IoErr::Closed => ReadErr::Closed,
            IoErr::Budget => ReadErr::Budget,
        }
    }
}

/// The per-connection read state: bytes already received beyond the last request (pipelining).
pub struct Conn<'b> {
    pub head: &'b mut [u8],
    pub have: usize,
}

fn find_head_end(b: &[u8]) -> Option<usize> {
    b.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

impl<'b> Conn<'b> {
    /// Reads the next request; its body goes into `body`. The returned Req borrows both buffers.
    pub fn next<'s, S: Stream>(&'s mut self, s: &mut S, body: &'s mut [u8]) -> Result<Req<'s>, ReadErr> {
        // wait for the start of a request (idle), then for the rest (request timeout)
        let idle = self.have == 0;
        let _ = s.sock().set_read_timeout(Some(Duration::from_millis(if idle { IDLE_MS } else { REQUEST_MS })));
        let start = crate::util::mono_ms();
        let head_end = loop {
            if let Some(e) = find_head_end(&self.head[..self.have]) {
                break e;
            }
            if self.have == self.head.len() {
                return Err(ReadErr::Status(431, "request head too large"));
            }
            if crate::util::mono_ms() - start > REQUEST_MS {
                return Err(ReadErr::Closed);
            }
            let n = s.read_some(&mut self.head[self.have..])?;
            if n == 0 {
                return Err(ReadErr::Closed);
            }
            if self.have == 0 {
                let _ = s.sock().set_read_timeout(Some(Duration::from_millis(REQUEST_MS)));
            }
            self.have += n;
        };
        let head = core::str::from_utf8(&self.head[..head_end]).map_err(|_| ReadErr::Status(400, "bad request"))?;
        let mut lines = head.split("\r\n");
        let first = lines.next().unwrap_or("");
        let mut parts = first.split(' ');
        let method = parts.next().unwrap_or("");
        let target = parts.next().unwrap_or("");
        let version = parts.next().unwrap_or("");
        if method.is_empty() || !target.starts_with('/') || !version.starts_with("HTTP/1.") {
            return Err(ReadErr::Status(400, "bad request"));
        }
        let mut close = version == "HTTP/1.0";
        let mut clen: Option<usize> = None;
        for l in lines {
            let Some((k, v)) = l.split_once(':') else { continue };
            let v = v.trim();
            if k.eq_ignore_ascii_case("content-length") {
                clen = Some(v.parse().map_err(|_| ReadErr::Status(400, "bad content-length"))?);
            } else if k.eq_ignore_ascii_case("transfer-encoding") {
                return Err(ReadErr::Status(411, "send a content-length"));
            } else if k.eq_ignore_ascii_case("connection") {
                if v.eq_ignore_ascii_case("close") {
                    close = true;
                } else if v.eq_ignore_ascii_case("keep-alive") {
                    close = false;
                }
            }
        }
        let clen = clen.unwrap_or(0);
        // the body buffer also holds copies of method, path and query (up to 1040 bytes) after the body
        if clen + 1040 > body.len() {
            return Err(ReadErr::Status(413, "body too large"));
        }
        // body: what came with the head, then the rest from the stream
        let extra = self.have - head_end;
        let from_head = extra.min(clen);
        body[..from_head].copy_from_slice(&self.head[head_end..head_end + from_head]);
        let mut got = from_head;
        while got < clen {
            if crate::util::mono_ms() - start > REQUEST_MS {
                return Err(ReadErr::Closed);
            }
            let n = s.read_some(&mut body[got..clen])?;
            if n == 0 {
                return Err(ReadErr::Closed);
            }
            got += n;
        }
        // method/target must outlive the head buffer's reuse: copy them out before shifting
        let consumed = head_end + from_head;
        let (method_s, path_s, query_s) = {
            let (p, q) = target.split_once('?').unwrap_or((target, ""));
            (Str::<16>::lossy(method), Str::<512>::lossy(p), Str::<512>::lossy(q))
        };
        self.head.copy_within(consumed..self.have, 0);
        self.have -= consumed;
        // keep the copies in the tail of `body` (after the body itself) so the Req can borrow them
        let tail = &mut body[clen..];
        let need = method_s.len() + path_s.len() + query_s.len();
        if tail.len() < need {
            return Err(ReadErr::Status(413, "body too large"));
        }
        let (m, rest) = tail.split_at_mut(method_s.len());
        m.copy_from_slice(method_s.as_bytes());
        let (p, rest) = rest.split_at_mut(path_s.len());
        p.copy_from_slice(path_s.as_bytes());
        let (q, _) = rest.split_at_mut(query_s.len());
        q.copy_from_slice(query_s.as_bytes());
        let (body_part, tail) = body.split_at(clen);
        let (m, rest) = tail.split_at(method_s.len());
        let (p, rest) = rest.split_at(path_s.len());
        let q = &rest[..query_s.len()];
        Ok(Req {
            method: core::str::from_utf8(m).unwrap_or(""),
            path: core::str::from_utf8(p).unwrap_or(""),
            query: core::str::from_utf8(q).unwrap_or(""),
            body: body_part,
            close,
        })
    }
}

pub fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Status",
    }
}

/// Writes a complete response (JSON body).
pub fn respond<S: Stream>(s: &mut S, status: u16, body: &[u8], close: bool) -> IoRes<()> {
    let _ = s.sock().set_write_timeout(Some(Duration::from_millis(REQUEST_MS)));
    let mut mem = [0u8; 256];
    let mut h = Buf::new(&mut mem);
    let _ = write!(
        h,
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n{}\r\n",
        reason(status),
        body.len(),
        if close { "connection: close\r\n" } else { "connection: keep-alive\r\nkeep-alive: timeout=5\r\n" }
    );
    s.write_all_bytes(h.as_bytes())?;
    s.write_all_bytes(body)
}

/// {"error": msg}
pub fn error_body<'a>(buf: &'a mut [u8], msg: &str) -> &'a [u8] {
    let mut b = Buf::new(buf);
    crate::json::W::new(&mut b).obj().kstr("error", msg).end();
    let n = b.len;
    &buf[..n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cn_of_a_real_certificate() {
        // openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj "/O=x/CN=sandbox-controller"
        let pem = include_str!("../tests/data/cn-test.crt");
        let der = CertificateDer::from_pem_slice(pem.as_bytes()).unwrap();
        assert_eq!(subject_cn(der.as_ref()), Some("sandbox-controller"));
        assert_eq!(subject_cn(&[0x30, 0x03, 0x02]), None);
        assert_eq!(subject_cn(&[]), None);
    }
}

#[cfg(test)]
mod budget_tests {
    //! Arena exhaustion: a TLS client that would make the server's per-connection state exceed its arena is refused
    //! (IoErr::Budget) without a crash, the arena is empty again afterwards, and a normal client still works.
    use super::*;
    use crate::alloc;
    use rustls::pki_types::ServerName;
    use std::net::TcpListener;

    fn pki() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("sbx-tls-test-{}", std::process::id()));
        let script = concat!(env!("CARGO_MANIFEST_DIR"), "/../host/make-certs.sh");
        let ok = std::process::Command::new("bash").arg(script).arg(&d).arg("hub").arg("controller").output().unwrap();
        assert!(ok.status.success(), "{}", String::from_utf8_lossy(&ok.stderr));
        d
    }

    fn client(d: &std::path::Path, extra_chain: usize, port: u16) -> std::io::Result<()> {
        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from_pem_file(d.join("ca.crt")).unwrap()).unwrap();
        let mut chain = vec![CertificateDer::from_pem_file(d.join("controller.crt")).unwrap()];
        for _ in 0..extra_chain {
            chain.push(CertificateDer::from_pem_file(d.join("ca.crt")).unwrap());
        }
        let key = PrivateKeyDer::from_pem_file(d.join("controller.key")).unwrap();
        let cfg = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .with_root_certificates(roots)
            .with_client_auth_cert(chain, key)
            .unwrap();
        let mut c = rustls::ClientConnection::new(Arc::new(cfg), ServerName::try_from("sandboxd").unwrap()).unwrap();
        let mut s = TcpStream::connect(("127.0.0.1", port))?;
        let mut st = rustls::Stream::new(&mut c, &mut s);
        st.write_all(b"GET /v1/host HTTP/1.1\r\n\r\n")?;
        let mut b = [0u8; 64];
        let _ = st.read(&mut b);
        Ok(())
    }

    #[test]
    fn exhaustion_is_an_error_not_a_crash() {
        let d = pki();
        let cfg = tls_config(
            d.join("hub.crt").to_str().unwrap(),
            d.join("hub.key").to_str().unwrap(),
            d.join("ca.crt").to_str().unwrap(),
        )
        .ok()
        .unwrap();
        // one arena of 256 KiB for the server thread (production: 512 KiB); the guard wants 200 KiB free before each call
        let size = 256 << 10;
        // (not from the heap: an arena inside the boot arena would be mistaken for it)
        let base = Box::leak(Box::new(crate::fixed::Region::map(size).unwrap())).raw(size);
        unsafe { alloc::init_arenas(base, 1, size) };
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let mut results = Vec::with_capacity(3);
            alloc::use_arena(1);
            for _ in 0..3 {
                let (s, _) = l.accept().unwrap();
                let r = Tls::accept(&cfg, s, crate::util::mono_ms() + 10_000).map(|t| {
                    let cn = t.peer_cn();
                    t.close();
                    cn
                });
                let used = alloc::current_usage().0;
                results.push((r.map(|cn| cn.map(|c| c.as_str() == CONTROLLER_CN)), used));
            }
            alloc::use_arena(0);
            results
        });
        let _ = client(&d, 0, port);
        let _ = client(&d, 120, port); // ~60 KiB certificate chain
        let _ = client(&d, 0, port);
        let r = server.join().unwrap();
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(r[0], (Ok(Some(true)), 0));
        assert_eq!(r[1], (Err(IoErr::Budget), 0), "the oversized handshake is refused and its memory freed");
        assert_eq!(r[2], (Ok(Some(true)), 0), "the next connection is fine");
        let s = alloc::stats();
        assert_eq!((s.overflow, s.failed), (0, 0));
        assert!(s.arena_peak <= size);
        // the allocator itself: past the worker arena it falls back to the boot arena (counted), and past that it
        // returns null (counted) instead of growing
        let t = std::thread::spawn(|| {
            use core::alloc::{GlobalAlloc, Layout};
            alloc::use_arena(1);
            let big = Layout::from_size_align(300 << 10, 8).unwrap();
            let p = unsafe { alloc::ArenaAlloc.alloc(big) };
            let huge = Layout::from_size_align(64 << 20, 8).unwrap();
            let q = unsafe { alloc::ArenaAlloc.alloc(huge) };
            if !p.is_null() {
                unsafe { alloc::ArenaAlloc.dealloc(p, big) };
            }
            alloc::use_arena(0);
            (p.is_null(), q.is_null())
        });
        assert_eq!(t.join().unwrap(), (false, true));
        let s = alloc::stats();
        assert_eq!((s.overflow, s.failed), (2, 1), "300 KiB: from the boot arena; 64 MiB: neither, so null");
    }
}

//! A frozen sandbox's memory is the pre-copy stream a migration sends (jarvis4 docs/PLAN-fabric.md, "How a frozen
//! image travels"): the fork's `runsc checkpoint --migrate-to` writes it and `runsc restore --migrate-listen` reads
//! it, and it only ever flows that one way. Two pipeline stages record and replay it:
//!
//! - `sandboxd stream-sink <listen ip:port> <timeout s>`: takes the one connection the checkpoint makes and copies the
//!   stream to stdout (a freeze: `stream-sink | sandboxd seal | aws s3 cp - <key>`);
//! - `sandboxd stream-source <restore ip:port> <timeout s>`: connects to the waiting restore and writes stdin to it (a
//!   thaw: `aws s3 cp <key> - | sandboxd open | stream-source`).
//!
//! Each checks the framing as it passes (relay.rs [`Parser`]): exit 0 only when the stream ended exactly at its End
//! record, so a cut or foreign stream never passes for a whole one.

use crate::relay::{MAGIC, Parser};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// Copies `r` to `w`, checking the framing; Err unless it was exactly one whole pre-copy stream.
pub fn copy_checked(r: &mut impl Read, w: &mut impl Write, buf: &mut [u8]) -> Result<u64, String> {
    let mut p = Parser::new();
    let mut total = 0u64;
    loop {
        let n = match r.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("reading: {e}")),
        };
        if total == 0 && n < MAGIC.len() && !MAGIC.starts_with(&buf[..n]) {
            return Err("not a pre-copy stream".into());
        }
        p.feed(&buf[..n]).map_err(str::to_owned)?;
        w.write_all(&buf[..n]).map_err(|e| format!("writing: {e}"))?;
        total += n as u64;
    }
    w.flush().map_err(|e| format!("writing: {e}"))?;
    if p.end { Ok(total) } else { Err("the stream ended before its End record".into()) }
}

fn parse_args(args: &[String]) -> Option<(&str, Instant)> {
    let [addr, secs] = args else { return None };
    Some((addr.as_str(), Instant::now() + Duration::from_secs(secs.parse().ok()?)))
}

fn log(s: &str) {
    let _ = writeln!(std::io::stderr(), "{s}");
}

/// The stage's own 64 KiB buffer (stack: the daemon's boot arena is not for this).
const BUF: usize = 64 << 10;

/// `sandboxd stream-sink …`: the exit code.
pub fn sink_main(args: &[String]) -> i32 {
    let Some((addr, deadline)) = parse_args(args) else {
        log("usage: stream-sink <listen ip:port> <timeout s>");
        return 2;
    };
    let l = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("stream-sink: listen {addr}: {e}"));
            return 1;
        }
    };
    let _ = l.set_nonblocking(true);
    let mut conn = loop {
        match l.accept() {
            Ok((c, _)) => break c,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => {
                log(&format!("stream-sink: no connection: {e}"));
                return 1;
            }
        }
    };
    let _ = conn.set_nonblocking(false);
    let _ = conn.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1))));
    let mut buf = [0u8; BUF];
    let mut out = std::io::stdout().lock();
    match copy_checked(&mut conn, &mut out, &mut buf) {
        Ok(n) => {
            log(&format!("stream-sink: {n} bytes"));
            0
        }
        Err(e) => {
            log(&format!("stream-sink: {e}"));
            1
        }
    }
}

/// `sandboxd stream-source …`: the exit code. The restore may not be listening yet: connecting is retried until the
/// deadline.
pub fn source_main(args: &[String]) -> i32 {
    let Some((addr, deadline)) = parse_args(args) else {
        log("usage: stream-source <restore ip:port> <timeout s>");
        return 2;
    };
    let mut conn = loop {
        match TcpStream::connect(addr) {
            Ok(c) => break c,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                log(&format!("stream-source: connect {addr}: {e}"));
                return 1;
            }
        }
    };
    let _ =
        conn.set_write_timeout(Some(deadline.saturating_duration_since(Instant::now()).max(Duration::from_secs(1))));
    let mut buf = [0u8; BUF];
    let mut input = std::io::stdin().lock();
    let r = copy_checked(&mut input, &mut conn, &mut buf);
    let _ = conn.shutdown(std::net::Shutdown::Write);
    match r {
        Ok(n) => {
            log(&format!("stream-source: {n} bytes"));
            0
        }
        Err(e) => {
            log(&format!("stream-source: {e}"));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_whole_stream_passes_unchanged_and_a_cut_or_foreign_one_does_not() {
        let s = crate::relay::tests::stream();
        let mut buf = [0u8; 7];
        let mut out = Vec::new();
        assert_eq!(copy_checked(&mut &s[..], &mut out, &mut buf).unwrap(), s.len() as u64);
        assert_eq!(out, s);
        for cut in [3, 8, 100, s.len() - 1] {
            let mut out = Vec::new();
            assert!(copy_checked(&mut &s[..cut], &mut out, &mut buf).is_err(), "cut at {cut}");
        }
        let mut longer = s.clone();
        longer.push(0);
        assert!(copy_checked(&mut &longer[..], &mut Vec::new(), &mut buf).is_err(), "bytes after End");
        assert!(copy_checked(&mut &b"tar data, not a stream"[..], &mut Vec::new(), &mut buf).is_err());
        assert!(copy_checked(&mut &b""[..], &mut Vec::new(), &mut buf).is_err(), "nothing");
    }

    #[test]
    fn the_sink_records_what_the_checkpoint_sends_and_the_source_replays_it() {
        let s = crate::relay::tests::stream();
        // the checkpoint's side: connect to the sink and send
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let sent = s.clone();
        let t = std::thread::spawn(move || {
            let mut c = TcpStream::connect(addr).unwrap();
            c.write_all(&sent).unwrap();
        });
        let (mut conn, _) = l.accept().unwrap();
        let mut recorded = Vec::new();
        copy_checked(&mut conn, &mut recorded, &mut [0u8; 1000]).unwrap();
        t.join().unwrap();
        assert_eq!(recorded, s);
        // the restore's side: the replay reads back the same bytes
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut got = Vec::new();
            c.read_to_end(&mut got).unwrap();
            got
        });
        let mut c = TcpStream::connect(addr).unwrap();
        copy_checked(&mut &recorded[..], &mut c, &mut [0u8; 1000]).unwrap();
        c.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(t.join().unwrap(), s);
    }
}

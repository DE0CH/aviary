//! S3 for a frozen sandbox's two objects (jarvis4 docs/PLAN-fabric.md, "How a frozen image travels"), with no AWS CLI
//! on the host: `sandboxd s3-put s3://<bucket>/<key>` uploads stdin as a multipart upload of unknown length (64 MiB
//! parts, each held in this stage's own memory, never the daemon's), `sandboxd s3-get s3://<bucket>/<key>` writes the
//! object to stdout. Requests are signed here (SigV4, [`sign`]: pure, checked against AWS's published example) and sent
//! with curl, as the host's image fetches are. Credentials come from the stage's environment
//! (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_DEFAULT_REGION`): short-lived ones for
//! these objects only. `SBX_S3_ENDPOINT` (tests) names another endpoint, addressed path-style.

use ring::hmac;

/// What a request is signed with.
pub struct Creds<'a> {
    pub access_key_id: &'a str,
    pub secret_access_key: &'a str,
    pub session_token: Option<&'a str>,
    pub region: &'a str,
}

/// The hex of SHA-256 over `b`.
pub fn sha256_hex(b: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, b).as_ref())
}

fn hex(b: &[u8]) -> String {
    use core::fmt::Write as _;
    let mut s = String::with_capacity(b.len() * 2);
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

fn hmac(key: &[u8], msg: &[u8]) -> Vec<u8> {
    hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), msg).as_ref().to_vec()
}

/// RFC 3986 encoding as SigV4 wants it: unreserved characters as they are, `/` too when `keep_slash`.
pub fn uri_encode(s: &str, keep_slash: bool) -> String {
    use core::fmt::Write as _;
    let mut o = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        if c.is_ascii_alphanumeric() || b"-_.~".contains(&c) || (keep_slash && c == b'/') {
            o.push(c as char);
        } else {
            let _ = write!(o, "%{c:02X}");
        }
    }
    o
}

/// The `Authorization` header of a request: `method` `path` (already absolute, not yet encoded), `query` pairs (not
/// yet encoded), `headers` (lowercase names, `host` among them; every one is signed), the payload's hash (or
/// `UNSIGNED-PAYLOAD`) and the time (`YYYYMMDDTHHMMSSZ`).
pub fn sign(
    method: &str,
    path: &str,
    query: &[(&str, &str)],
    headers: &[(&str, &str)],
    payload_hash: &str,
    amz_date: &str,
    c: &Creds<'_>,
) -> String {
    let mut q: Vec<(String, String)> =
        query.iter().map(|(k, v)| (uri_encode(k, false), uri_encode(v, false))).collect();
    q.sort();
    let cq = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let mut h: Vec<(String, String)> =
        headers.iter().map(|(k, v)| (k.to_ascii_lowercase(), v.trim().to_owned())).collect();
    h.sort();
    let ch = h.iter().fold(String::new(), |mut o, (k, v)| {
        use core::fmt::Write as _;
        let _ = writeln!(o, "{k}:{v}");
        o
    });
    let signed = h.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
    let canonical = format!("{method}\n{}\n{cq}\n{ch}\n{signed}\n{payload_hash}", uri_encode(path, true));
    let date = &amz_date[..8];
    let scope = format!("{date}/{}/s3/aws4_request", c.region);
    let to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha256_hex(canonical.as_bytes()));
    let k = hmac(format!("AWS4{}", c.secret_access_key).as_bytes(), date.as_bytes());
    let k = hmac(&k, c.region.as_bytes());
    let k = hmac(&k, b"s3");
    let k = hmac(&k, b"aws4_request");
    let sig = hex(&hmac(&k, to_sign.as_bytes()));
    format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed}, Signature={sig}", c.access_key_id)
}

// ---- the stages -----------------------------------------------------------------------------------------------------

/// One part of an upload (S3 allows 10 000: 640 GiB, beyond any sandbox's memory and scratch).
const PART: usize = 64 << 20;

/// Where an object is: the URL curl asks, the `host` it sends, the path signed.
struct Target {
    url_base: String,
    host: String,
    path: String,
}

fn target(object: &str, region: &str, endpoint: Option<&str>) -> Result<Target, String> {
    let rest = object.strip_prefix("s3://").ok_or("not an s3:// URL")?;
    let (bucket, key) =
        rest.split_once('/').filter(|(b, k)| !b.is_empty() && !k.is_empty()).ok_or("no bucket or key")?;
    Ok(if let Some(e) = endpoint {
        let host = e.split_once("://").map_or(e, |(_, h)| h).trim_end_matches('/').to_owned();
        let path = format!("/{bucket}/{key}");
        Target { url_base: format!("{}{}", e.trim_end_matches('/'), uri_encode(&path, true)), host, path }
    } else {
        let host = format!("{bucket}.s3.{region}.amazonaws.com");
        let path = format!("/{key}");
        Target { url_base: format!("https://{host}{}", uri_encode(&path, true)), host, path }
    })
}

fn amz_now() -> String {
    // ISO 8601 basic, as SigV4 wants: from util::iso's 2026-11-17T18:54:16.956Z
    let iso = crate::util::iso(crate::util::now_ms());
    let s: String = iso.as_str().chars().filter(|c| c.is_ascii_digit() || *c == 'T').collect();
    format!("{}Z", &s[..15])
}

/// One signed request with curl; `body` goes on its stdin. The status and the response body.
fn request(
    c: &Creds<'_>,
    t: &Target,
    method: &str,
    query: &[(&str, &str)],
    body: Option<&[u8]>,
    out: &mut dyn std::io::Write,
) -> Result<(u16, Vec<u8>), String> {
    request_inner(c, t, method, query, body, out, false)
}

/// [`request`], with the response's headers before its body when `with_headers` (curl `-i`). The body (all of it
/// passes through `out` but the last 64 bytes, which come back with the status).
fn request_inner(
    c: &Creds<'_>,
    t: &Target,
    method: &str,
    query: &[(&str, &str)],
    body: Option<&[u8]>,
    out: &mut dyn std::io::Write,
    with_headers: bool,
) -> Result<(u16, Vec<u8>), String> {
    use std::io::{Read as _, Write as _};
    let date = amz_now();
    let mut headers: Vec<(&str, &str)> =
        vec![("host", &t.host), ("x-amz-content-sha256", "UNSIGNED-PAYLOAD"), ("x-amz-date", &date)];
    if let Some(tok) = c.session_token {
        headers.push(("x-amz-security-token", tok));
    }
    let auth = sign(method, &t.path, query, &headers, "UNSIGNED-PAYLOAD", &date, c);
    let mut url = t.url_base.clone();
    if !query.is_empty() {
        url.push('?');
        url.push_str(
            &query
                .iter()
                .map(|(k, v)| {
                    if v.is_empty() && *k == "uploads" {
                        (*k).to_owned()
                    } else {
                        format!("{}={}", uri_encode(k, false), uri_encode(v, false))
                    }
                })
                .collect::<Vec<_>>()
                .join("&"),
        );
    }
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-sS", "-X", method, "-w", "\n%{http_code}", "--max-time", "900"]);
    if with_headers {
        cmd.arg("-i");
    }
    if method == "GET" {
        cmd.arg("--fail");
    }
    for (k, v) in headers.iter().filter(|(k, _)| *k != "host") {
        cmd.arg("-H").arg(format!("{k}: {v}"));
    }
    cmd.arg("-H").arg(format!("Authorization: {auth}"));
    if let Some(b) = body {
        cmd.args(["--data-binary", "@-", "-H", "Content-Type:"]);
        cmd.arg("-H").arg(format!("Content-Length: {}", b.len()));
    }
    cmd.arg(&url);
    cmd.stdin(if body.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() });
    cmd.stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("curl: {e}"))?;
    if let (Some(b), Some(mut i)) = (body, child.stdin.take()) {
        i.write_all(b).map_err(|e| format!("curl's stdin: {e}"))?;
    }
    // the response: a GET's body streams to `out`; the last line is the status curl adds
    let mut so = child.stdout.take().ok_or("curl's stdout")?;
    let mut held: Vec<u8> = Vec::new();
    let mut buf = vec![0u8; 1 << 16];
    let streaming = method == "GET";
    loop {
        let n = so.read(&mut buf).map_err(|e| format!("curl's stdout: {e}"))?;
        if n == 0 {
            break;
        }
        held.extend_from_slice(&buf[..n]);
        // a GET's body streams on (curl --fail: an error answer never reaches it), keeping the tail where curl
        // writes the status; any other answer is small and kept whole
        if streaming && held.len() > 64 {
            let keep = held.len() - 64;
            out.write_all(&held[..keep]).map_err(|e| format!("writing: {e}"))?;
            held.drain(..keep);
        }
    }
    let st = child.wait().map_err(|e| format!("curl: {e}"))?;
    // curl --fail: 22 is an error answer (its status follows on stdout all the same)
    if !st.success() && st.code() != Some(22) {
        return Err(format!("curl exited {st}"));
    }
    let nl = held.iter().rposition(|b| *b == b'\n').ok_or("no status from curl")?;
    let code: u16 =
        std::str::from_utf8(&held[nl + 1..]).ok().and_then(|s| s.trim().parse().ok()).ok_or("no status from curl")?;
    held.truncate(nl);
    Ok((code, held))
}

/// An error answer in a line: S3's `Code: Message`, else the start of the body.
fn why(code: u16, body: &[u8]) -> String {
    match (xml_tag(body, "Code"), xml_tag(body, "Message")) {
        (Some(c), Some(m)) => format!("HTTP {code} {c}: {m}"),
        (Some(c), None) => format!("HTTP {code} {c}"),
        _ => format!("HTTP {code} {}", String::from_utf8_lossy(&body[..body.len().min(300)]).trim()),
    }
}

fn xml_tag(body: &[u8], tag: &str) -> Option<String> {
    let s = std::str::from_utf8(body).ok()?;
    let a = s.find(&format!("<{tag}>"))? + tag.len() + 2;
    let b = s[a..].find(&format!("</{tag}>"))? + a;
    Some(s[a..b].to_owned())
}

/// Reads until `buf` is full or the input ends.
fn fill(r: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// stdin to `object`, multipart; the upload is aborted on any failure, so a half object never exists.
fn put(c: &Creds<'_>, t: &Target, input: &mut impl std::io::Read, part: &mut [u8]) -> Result<u64, String> {
    let mut sink = std::io::sink();
    let (code, body) = request(c, t, "POST", &[("uploads", "")], None, &mut sink)?;
    let (200, Some(id)) = (code, xml_tag(&body, "UploadId")) else {
        return Err(format!("starting the upload: {}", why(code, &body)));
    };
    let abort = |why: String| {
        let _ = request(c, t, "DELETE", &[("uploadId", &id)], None, &mut std::io::sink());
        why
    };
    let mut parts: Vec<String> = Vec::new();
    let mut total = 0u64;
    loop {
        let n = fill(input, part).map_err(|e| abort(format!("reading: {e}")))?;
        if n == 0 && !parts.is_empty() {
            break;
        }
        let num = (parts.len() + 1).to_string();
        // the ETag is a response header: ask for headers on stdout with the body
        let etag = upload_part(c, t, &id, &num, &part[..n]).map_err(abort)?;
        parts.push(etag);
        total += n as u64;
        if n < part.len() {
            break;
        }
    }
    let mut xml = String::from("<CompleteMultipartUpload>");
    for (i, e) in parts.iter().enumerate() {
        use core::fmt::Write as _;
        let _ = write!(xml, "<Part><PartNumber>{}</PartNumber><ETag>{e}</ETag></Part>", i + 1);
    }
    xml.push_str("</CompleteMultipartUpload>");
    let (code, body) = request(c, t, "POST", &[("uploadId", &id)], Some(xml.as_bytes()), &mut sink)?;
    // S3 can answer 200 with an error in the body
    if code != 200 || xml_tag(&body, "Code").is_some() {
        return Err(abort(format!("completing the upload: {}", why(code, &body))));
    }
    Ok(total)
}

/// One part; its ETag (from the response headers, which curl prints with `-i`).
fn upload_part(c: &Creds<'_>, t: &Target, id: &str, num: &str, data: &[u8]) -> Result<String, String> {
    // a response with headers: request() passes the body through; here the headers are the answer
    let mut got: Vec<u8> = Vec::new();
    let t2 = Target { url_base: t.url_base.clone(), host: t.host.clone(), path: t.path.clone() };
    let (code, rest) = request_with_headers(c, &t2, &[("partNumber", num), ("uploadId", id)], data, &mut got)?;
    if code != 200 {
        return Err(format!("part {num}: {}", why(code, &rest)));
    }
    let head = String::from_utf8_lossy(&got).into_owned() + &String::from_utf8_lossy(&rest);
    head.lines()
        .find_map(|l| {
            l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("etag")).map(|(_, v)| v.trim().to_owned())
        })
        .ok_or_else(|| format!("part {num}: no ETag"))
}

/// A PUT whose response headers are wanted (`-i`): they come through `out` with the body.
fn request_with_headers(
    c: &Creds<'_>,
    t: &Target,
    query: &[(&str, &str)],
    body: &[u8],
    out: &mut Vec<u8>,
) -> Result<(u16, Vec<u8>), String> {
    // `-i` is passed by marking the method; request() adds it for PUT parts
    request_inner(c, t, "PUT", query, Some(body), out, true)
}

/// `sandboxd s3-put|s3-get <object>`: the exit code (0, or 1 with the reason on stderr).
pub fn main(get: bool, args: &[String]) -> i32 {
    let name = if get { "s3-get" } else { "s3-put" };
    let [object] = args else {
        eprintln!("usage: sandboxd {name} s3://<bucket>/<key>");
        return 2;
    };
    let [Some(id), Some(secret), token, region, endpoint] = crate::env::s3_env() else {
        eprintln!("sandboxd {name}: AWS_ACCESS_KEY_ID and AWS_SECRET_ACCESS_KEY are needed");
        return 2;
    };
    let region = region.unwrap_or_else(|| "us-east-1".to_owned());
    let c = Creds { access_key_id: &id, secret_access_key: &secret, session_token: token.as_deref(), region: &region };
    let t = match target(object, &region, endpoint.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("sandboxd {name}: {object}: {e}");
            return 2;
        }
    };
    let r = if get {
        let mut out = std::io::stdout().lock();
        match request(&c, &t, "GET", &[], None, &mut out) {
            Ok((200, rest)) => {
                use std::io::Write as _;
                out.write_all(&rest).and_then(|()| out.flush()).map_err(|e| e.to_string()).map(|()| 0)
            }
            Ok((code, rest)) => Err(why(code, &rest)),
            Err(e) => Err(e),
        }
    } else {
        // the part buffer, mapped here (this process only)
        // SAFETY: an anonymous private mapping, checked for MAP_FAILED below, used by this process only.
        let p = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                PART,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            eprintln!("sandboxd s3-put: no memory for a part");
            return 1;
        }
        // SAFETY: the mapping is PART bytes, readable and writable, and lives until the process ends.
        let part = unsafe { core::slice::from_raw_parts_mut(p.cast::<u8>(), PART) };
        put(&c, &t, &mut std::io::stdin().lock(), part).map(|n| {
            eprintln!("sandboxd s3-put: {n} bytes");
            0
        })
    };
    r.unwrap_or_else(|e| {
        eprintln!("sandboxd {name}: {e}");
        1
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS's own example (S3 docs, "Signature Calculations for the Authorization Header", GET Object).
    #[test]
    fn the_signature_is_aws_examples() {
        let c = Creds {
            access_key_id: "AKIAIOSFODNN7EXAMPLE",
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            session_token: None,
            region: "us-east-1",
        };
        let empty = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
        assert_eq!(sha256_hex(b""), empty);
        let a = sign(
            "GET",
            "/test.txt",
            &[],
            &[
                ("host", "examplebucket.s3.amazonaws.com"),
                ("range", "bytes=0-9"),
                ("x-amz-content-sha256", empty),
                ("x-amz-date", "20130524T000000Z"),
            ],
            empty,
            "20130524T000000Z",
            &c,
        );
        assert_eq!(
            a,
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date, \
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn keys_and_queries_are_encoded_as_sigv4_wants() {
        assert_eq!(uri_encode("frozen/sbx-1/01a1.scratch", true), "frozen/sbx-1/01a1.scratch");
        assert_eq!(uri_encode("a b+c/d", false), "a%20b%2Bc%2Fd");
        assert_eq!(uri_encode("uploads", false), "uploads");
    }
}

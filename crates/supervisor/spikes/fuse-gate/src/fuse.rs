//! The spike FUSE server: `~/.claude` served from the fake magpie, write-through with a local content cache, a
//! node-id log that survives restarts, the `/dev/fuse` fd held by the keeper, and the request journal (every request
//! is read straight into a slot of a MAP_SHARED file, replayed by the next process after a crash).

use crate::http::{Client, pct_encode};
use crate::keeper;
use crate::magpie::now_us;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::RawFd;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const SLOT: usize = (1 << 20) + 8192;
const MAX_WRITE: u32 = 1 << 20;

const ENOENT: i32 = 2;
const EIO: i32 = 5;
const EPERM: i32 = 1;
const EEXIST: i32 = 17;
const EINVAL: i32 = 22;
const EFBIG: i32 = 27;
const ENOSYS: i32 = 38;
const ENOTEMPTY: i32 = 39;
const ENODATA: i32 = 61;
const ENOTSUP: i32 = 95;
const EDQUOT: i32 = 122;

pub fn opname(op: u32) -> &'static str {
    match op {
        1 => "LOOKUP",
        2 => "FORGET",
        3 => "GETATTR",
        4 => "SETATTR",
        5 => "READLINK",
        6 => "SYMLINK",
        8 => "MKNOD",
        9 => "MKDIR",
        10 => "UNLINK",
        11 => "RMDIR",
        12 => "RENAME",
        13 => "LINK",
        14 => "OPEN",
        15 => "READ",
        16 => "WRITE",
        17 => "STATFS",
        18 => "RELEASE",
        20 => "FSYNC",
        21 => "SETXATTR",
        22 => "GETXATTR",
        23 => "LISTXATTR",
        24 => "REMOVEXATTR",
        25 => "FLUSH",
        26 => "INIT",
        27 => "OPENDIR",
        28 => "READDIR",
        29 => "RELEASEDIR",
        30 => "FSYNCDIR",
        31 => "GETLK",
        32 => "SETLK",
        33 => "SETLKW",
        34 => "ACCESS",
        35 => "CREATE",
        36 => "INTERRUPT",
        38 => "DESTROY",
        39 => "IOCTL",
        40 => "POLL",
        42 => "BATCH_FORGET",
        43 => "FALLOCATE",
        44 => "READDIRPLUS",
        45 => "RENAME2",
        46 => "LSEEK",
        47 => "COPY_FILE_RANGE",
        _ => "UNKNOWN",
    }
}

pub fn opcode_of(name: &str) -> u32 {
    (0..64).find(|&o| opname(o) == name).unwrap_or(0)
}

#[derive(Clone)]
struct Node {
    path: String,
    dir: bool,
    mode: u32,
    size: u64,
    mtime: u64,
    ctime: u64,
    gone: bool,
}

struct Inner {
    nodes: HashMap<u64, Node>,
    by_path: HashMap<String, u64>,
    next: u64,
    nlog: File,
}

impl Inner {
    fn assign(&mut self, path: &str, dir: bool, mode: u32, size: u64, mtime: u64) -> u64 {
        if let Some(&i) = self.by_path.get(path) {
            let n = self.nodes.get_mut(&i).expect("node");
            n.dir = dir;
            n.mode = mode;
            n.size = size;
            n.mtime = mtime;
            n.gone = false;
            return i;
        }
        let i = self.next;
        self.next += 1;
        let _ = writeln!(self.nlog, "a {i} {path}");
        self.nodes.insert(i, Node { path: path.to_string(), dir, mode, size, mtime, ctime: mtime, gone: false });
        self.by_path.insert(path.to_string(), i);
        i
    }
    fn drop_path(&mut self, path: &str) {
        if let Some(i) = self.by_path.remove(path) {
            if let Some(n) = self.nodes.get_mut(&i) {
                n.gone = true;
            }
            let _ = writeln!(self.nlog, "d {i}");
        }
    }
}

pub struct Die {
    pub point: String,
    pub opcode: u32,
    pub nth: u64,
    pub seen: AtomicU64,
}

pub struct Fs {
    inner: Mutex<Inner>,
    boot: u64,
    cache: PathBuf,
    oplog: Mutex<File>,
    die: Option<Die>,
    fd: RawFd,
    magpie: String,
}

pub struct Opts {
    pub mount: String,
    pub magpie: String,
    pub state: PathBuf,
    pub journal: PathBuf,
    pub keeper: String,
    pub workers: usize,
    pub die: Option<Die>,
}

fn now_ns() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64)
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

fn parent_of(p: &str) -> &str {
    p.rsplit_once('/').map_or("", |(a, _)| a)
}

fn u32at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn u64at(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn cstr(b: &[u8]) -> (&str, &[u8]) {
    let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    (std::str::from_utf8(&b[..n]).unwrap_or(""), &b[(n + 1).min(b.len())..])
}

fn attr(ino: u64, n: &Node, o: &mut Vec<u8>) {
    let s = n.mtime / 1_000_000_000;
    let ns = (n.mtime % 1_000_000_000) as u32;
    let cs = n.ctime / 1_000_000_000;
    let cns = (n.ctime % 1_000_000_000) as u32;
    for v in [ino, n.size, n.size.div_ceil(512), s, s, cs] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    let mode = if n.dir { libc::S_IFDIR | n.mode } else { libc::S_IFREG | n.mode };
    for v in [ns, ns, cns, mode, if n.dir { 2 } else { 1 }, 1000, 1000, 0, 4096, 0] {
        o.extend_from_slice(&v.to_le_bytes());
    }
}

fn entry(ino: u64, n: &Node) -> Vec<u8> {
    let mut o = Vec::with_capacity(128);
    for v in [ino, 0u64, 1, 1] {
        o.extend_from_slice(&v.to_le_bytes());
    }
    o.extend_from_slice(&0u32.to_le_bytes());
    o.extend_from_slice(&0u32.to_le_bytes());
    attr(ino, n, &mut o);
    o
}

fn attr_out(ino: u64, n: &Node) -> Vec<u8> {
    let mut o = Vec::with_capacity(104);
    o.extend_from_slice(&1u64.to_le_bytes());
    o.extend_from_slice(&0u32.to_le_bytes());
    o.extend_from_slice(&0u32.to_le_bytes());
    attr(ino, n, &mut o);
    o
}

fn open_out(fh: u64) -> Vec<u8> {
    let mut o = fh.to_le_bytes().to_vec();
    o.extend_from_slice(&[0u8; 8]);
    o
}

fn reply(unique: u64, err: i32, payload: &[u8]) -> Vec<u8> {
    let mut o = Vec::with_capacity(16 + payload.len());
    o.extend_from_slice(&((16 + payload.len()) as u32).to_le_bytes());
    o.extend_from_slice(&(-err).to_le_bytes());
    o.extend_from_slice(&unique.to_le_bytes());
    o.extend_from_slice(payload);
    o
}

fn errno_of(code: u16, body: &[u8]) -> i32 {
    match (code, body) {
        (404, _) => ENOENT,
        (403, _) => EPERM,
        (409, b"exists") => EEXIST,
        (409, b"not_empty") => ENOTEMPTY,
        (413, _) => EFBIG,
        (507, _) => EDQUOT,
        (400, _) => EINVAL,
        _ => EIO,
    }
}

struct Ctx {
    opcode: u32,
    marked: bool,
}

impl Fs {
    fn maybe_die(&self, point: &str, ctx: &Ctx) {
        if let Some(d) = &self.die {
            if ctx.marked && d.point == point {
                eprintln!("fault-injection: SIGKILL at {point} ({})", opname(ctx.opcode));
                // SAFETY: kill(2) on ourselves.
                unsafe { libc::kill(libc::getpid(), libc::SIGKILL) };
            }
        }
    }

    /// One magpie call with the retry rule (503 / connection error: retry with backoff, same op id).
    fn call(&self, cl: &mut Client, target: &str, body: &[u8], ctx: &Ctx) -> Result<Vec<u8>, i32> {
        let mut wait = 200u64;
        loop {
            let res = cl.call("POST", target, body, &mut || self.maybe_die("sent", ctx));
            match res {
                Ok((200, b)) => {
                    self.maybe_die("answered", ctx);
                    return Ok(b);
                }
                Ok((503, _)) | Err(_) => {
                    thread::sleep(Duration::from_millis(wait));
                    wait = (wait * 2).min(10_000);
                }
                Ok((c, b)) => {
                    self.maybe_die("answered", ctx);
                    return Err(errno_of(c, &b));
                }
            }
        }
    }

    fn op(&self, unique: u64, step: u32) -> String {
        format!("{:016x}-{unique:016x}-{step}", self.boot)
    }

    fn node(&self, ino: u64) -> Option<Node> {
        self.inner.lock().unwrap().nodes.get(&ino).cloned()
    }

    fn cache_file(&self, ino: u64) -> PathBuf {
        self.cache.join(ino.to_string())
    }

    /// Makes sure the content cache holds the node's bytes; fetches the whole file on a miss.
    fn ensure_cached(&self, cl: &mut Client, ino: u64, n: &Node, ctx: &Ctx) -> Result<(), i32> {
        let cf = self.cache_file(ino);
        if fs::metadata(&cf).is_ok_and(|m| m.len() == n.size) {
            return Ok(());
        }
        let mut wait = 200;
        let body = loop {
            match cl.call("GET", &format!("/file?path={}", pct_encode(&n.path)), &[], &mut || self.maybe_die("sent", ctx)) {
                Ok((200, b)) => {
                    self.maybe_die("answered", ctx);
                    break b;
                }
                Ok((503, _)) | Err(_) => {
                    thread::sleep(Duration::from_millis(wait));
                    wait = (wait * 2).min(10_000);
                }
                Ok((c, b)) => return Err(errno_of(c, &b)),
            }
        };
        fs::write(&cf, &body).map_err(|_| EIO)
    }

    fn handle(&self, cl: &mut Client, req: &[u8], marked: bool) -> Option<Vec<u8>> {
        let len = u32at(req, 0) as usize;
        let opcode = u32at(req, 4);
        let unique = u64at(req, 8);
        let ino = u64at(req, 16);
        let body = &req[40..len.min(req.len())];
        let ctx = Ctx { opcode, marked };
        let t0 = now_us();
        let mut detail = String::new();
        let res: Option<Result<Vec<u8>, i32>> = self.dispatch(cl, opcode, unique, ino, body, &ctx, &mut detail);
        let path = self.node(ino).map(|n| n.path).unwrap_or_default();
        let err = match &res {
            Some(Err(e)) => *e,
            _ => 0,
        };
        let line = format!("{t0}\t{}\t{ino}\t{path}\t{detail}\t{err}\t{}\t{unique}\n", opname(opcode), now_us() - t0);
        let _ = self.oplog.lock().unwrap().write_all(line.as_bytes());
        match res {
            None => None,
            Some(Ok(p)) => Some(reply(unique, 0, &p)),
            Some(Err(e)) => Some(reply(unique, e, &[])),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &self,
        cl: &mut Client,
        opcode: u32,
        unique: u64,
        ino: u64,
        b: &[u8],
        ctx: &Ctx,
        detail: &mut String,
    ) -> Option<Result<Vec<u8>, i32>> {
        let mtime = now_ns();
        Some(match opcode {
            26 => {
                let minor = u32at(b, 4).min(31);
                let flags = u32at(b, 12) & ((1 << 3) | (1 << 5) | (1 << 22));
                *detail = format!("kernel {}.{} flags {:#x} -> {flags:#x}", u32at(b, 0), u32at(b, 4), u32at(b, 12));
                let mut o = Vec::with_capacity(64);
                for v in [7u32, minor, u32at(b, 8), flags] {
                    o.extend_from_slice(&v.to_le_bytes());
                }
                o.extend_from_slice(&64u16.to_le_bytes());
                o.extend_from_slice(&48u16.to_le_bytes());
                o.extend_from_slice(&MAX_WRITE.to_le_bytes());
                o.extend_from_slice(&1u32.to_le_bytes());
                o.extend_from_slice(&256u16.to_le_bytes());
                o.extend_from_slice(&[0u8; 2 + 32]);
                Ok(o)
            }
            2 | 42 | 36 => {
                if opcode == 36 {
                    *detail = format!("target {}", u64at(b, 0));
                }
                return None;
            }
            1 => {
                let (name, _) = cstr(b);
                *detail = name.to_string();
                let s = self.inner.lock().unwrap();
                let Some(p) = s.nodes.get(&ino) else { return Some(Err(ENOENT)) };
                let cp = join(&p.path, name);
                match s.by_path.get(&cp) {
                    Some(&ci) => Ok(entry(ci, &s.nodes[&ci])),
                    None => Err(ENOENT),
                }
            }
            3 => match self.node(ino) {
                Some(n) => Ok(attr_out(ino, &n)),
                None => Err(ENOENT),
            },
            4 => {
                let valid = u32at(b, 0);
                let Some(n) = self.node(ino) else { return Some(Err(ENOENT)) };
                *detail = format!("valid={valid:#x}");
                if valid & (2 | 4) != 0 {
                    let (uid, gid) = (u32at(b, 76), u32at(b, 80));
                    if (valid & 2 != 0 && uid != 1000) || (valid & 4 != 0 && gid != 1000) {
                        return Some(Err(EPERM));
                    }
                }
                let p = pct_encode(&n.path);
                if valid & 8 != 0 {
                    let size = u64at(b, 16);
                    detail.push_str(&format!(" size={size}"));
                    if let Err(e) = self.call(cl, &format!("/truncate?path={p}&size={size}&mtime={mtime}&op={}", self.op(unique, 0)), &[], ctx) {
                        return Some(Err(e));
                    }
                    let cf = self.cache_file(ino);
                    if fs::metadata(&cf).is_ok_and(|m| m.len() == n.size) {
                        let _ = OpenOptions::new().write(true).open(&cf).and_then(|f| f.set_len(size));
                    } else {
                        let _ = fs::remove_file(&cf);
                    }
                    let mut s = self.inner.lock().unwrap();
                    let m = s.nodes.get_mut(&ino).unwrap();
                    m.size = size;
                    m.mtime = mtime;
                    m.ctime = mtime;
                }
                if valid & 1 != 0 {
                    let mode = u32at(b, 68) & 0o7777;
                    if let Err(e) = self.call(cl, &format!("/setattr?path={p}&mode={mode}&op={}", self.op(unique, 1)), &[], ctx) {
                        return Some(Err(e));
                    }
                    let mut s = self.inner.lock().unwrap();
                    let m = s.nodes.get_mut(&ino).unwrap();
                    m.mode = mode;
                    m.ctime = mtime;
                }
                if valid & (32 | 256) != 0 {
                    let mt = if valid & 256 != 0 { mtime } else { u64at(b, 40) * 1_000_000_000 + u64::from(u32at(b, 60)) };
                    if let Err(e) = self.call(cl, &format!("/setattr?path={p}&mtime={mt}&op={}", self.op(unique, 2)), &[], ctx) {
                        return Some(Err(e));
                    }
                    let mut s = self.inner.lock().unwrap();
                    let m = s.nodes.get_mut(&ino).unwrap();
                    m.mtime = mt;
                    m.ctime = mtime;
                }
                Ok(attr_out(ino, &self.node(ino).unwrap()))
            }
            5 => Err(EINVAL),
            6 | 8 | 13 => Err(EPERM),
            9 => {
                let mode = u32at(b, 0) & 0o7777 & !u32at(b, 4);
                let (name, _) = cstr(&b[8..]);
                let Some(par) = self.node(ino) else { return Some(Err(ENOENT)) };
                let cp = join(&par.path, name);
                *detail = name.to_string();
                if let Err(e) = self.call(cl, &format!("/mkdir?path={}&mode={mode}&mtime={mtime}&op={}", pct_encode(&cp), self.op(unique, 0)), &[], ctx) {
                    return Some(Err(e));
                }
                let mut s = self.inner.lock().unwrap();
                let ci = s.assign(&cp, true, mode, 0, mtime);
                Ok(entry(ci, &s.nodes[&ci]))
            }
            10 | 11 => {
                let (name, _) = cstr(b);
                let Some(par) = self.node(ino) else { return Some(Err(ENOENT)) };
                let cp = join(&par.path, name);
                *detail = name.to_string();
                let route = if opcode == 10 { "unlink" } else { "rmdir" };
                if let Err(e) = self.call(cl, &format!("/{route}?path={}&op={}", pct_encode(&cp), self.op(unique, 0)), &[], ctx) {
                    return Some(Err(e));
                }
                self.inner.lock().unwrap().drop_path(&cp);
                Ok(vec![])
            }
            12 | 45 => {
                let newdir = u64at(b, 0);
                let rest = if opcode == 45 { &b[16..] } else { &b[8..] };
                let (old, rest) = cstr(rest);
                let (new, _) = cstr(rest);
                if opcode == 45 && u32at(b, 8) != 0 {
                    return Some(Err(EINVAL));
                }
                let (Some(pa), Some(pb)) = (self.node(ino), self.node(newdir)) else { return Some(Err(ENOENT)) };
                let from = join(&pa.path, old);
                let to = join(&pb.path, new);
                *detail = format!("{from} -> {to}");
                if let Err(e) = self.call(cl, &format!("/rename?path={}&to={}&op={}", pct_encode(&from), pct_encode(&to), self.op(unique, 0)), &[], ctx) {
                    return Some(Err(e));
                }
                let mut s = self.inner.lock().unwrap();
                if s.by_path.contains_key(&to) {
                    s.drop_path(&to);
                }
                if let Some(i) = s.by_path.remove(&from) {
                    let pre = format!("{from}/");
                    let kids: Vec<(String, u64)> =
                        s.by_path.iter().filter(|(k, _)| k.starts_with(&pre)).map(|(k, v)| (k.clone(), *v)).collect();
                    s.by_path.insert(to.clone(), i);
                    s.nodes.get_mut(&i).unwrap().path = to.clone();
                    let _ = writeln!(s.nlog, "r {i} {to}");
                    for (k, ki) in kids {
                        let np = format!("{to}/{}", &k[pre.len()..]);
                        s.by_path.remove(&k);
                        s.by_path.insert(np.clone(), ki);
                        s.nodes.get_mut(&ki).unwrap().path = np.clone();
                        let _ = writeln!(s.nlog, "r {ki} {np}");
                    }
                }
                Ok(vec![])
            }
            14 => {
                let flags = u32at(b, 0) as i32;
                let Some(n) = self.node(ino) else { return Some(Err(ENOENT)) };
                *detail = format!("flags={flags:#o}");
                if flags & libc::O_TRUNC != 0 && !n.dir && n.size > 0 {
                    if let Err(e) = self.call(cl, &format!("/truncate?path={}&size=0&mtime={mtime}&op={}", pct_encode(&n.path), self.op(unique, 0)), &[], ctx) {
                        return Some(Err(e));
                    }
                    let _ = File::create(self.cache_file(ino));
                    let mut s = self.inner.lock().unwrap();
                    let m = s.nodes.get_mut(&ino).unwrap();
                    m.size = 0;
                    m.mtime = mtime;
                }
                Ok(open_out(ino << 8))
            }
            27 => Ok(open_out(0)),
            15 => {
                let off = u64at(b, 8);
                let size = u64::from(u32at(b, 16));
                *detail = format!("off={off} size={size}");
                let Some(n) = self.node(ino) else { return Some(Err(ENOENT)) };
                if let Err(e) = self.ensure_cached(cl, ino, &n, ctx) {
                    return Some(Err(e));
                }
                let n_ = size.min(n.size.saturating_sub(off));
                let mut buf = vec![0u8; n_ as usize];
                if n_ > 0 {
                    match File::open(self.cache_file(ino)) {
                        Ok(f) => {
                            if f.read_exact_at(&mut buf, off).is_err() {
                                return Some(Err(EIO));
                            }
                        }
                        Err(_) => return Some(Err(EIO)),
                    }
                }
                Ok(buf)
            }
            16 => {
                let off = u64at(b, 8);
                let size = u32at(b, 16) as usize;
                let data = &b[40..40 + size];
                *detail = format!("off={off} size={size}");
                let Some(n) = self.node(ino) else { return Some(Err(ENOENT)) };
                if let Err(e) = self.call(cl, &format!("/write?path={}&offset={off}&mtime={mtime}&create=0&op={}", pct_encode(&n.path), self.op(unique, 0)), data, ctx) {
                    return Some(Err(e));
                }
                let cf = self.cache_file(ino);
                if fs::metadata(&cf).is_ok_and(|m| m.len() == n.size) {
                    let _ = OpenOptions::new().write(true).open(&cf).and_then(|f| f.write_all_at(data, off));
                } else {
                    let _ = fs::remove_file(&cf);
                }
                let mut s = self.inner.lock().unwrap();
                let m = s.nodes.get_mut(&ino).unwrap();
                m.size = m.size.max(off + size as u64);
                m.mtime = mtime;
                m.ctime = mtime;
                let mut o = (size as u32).to_le_bytes().to_vec();
                o.extend_from_slice(&[0; 4]);
                Ok(o)
            }
            17 => {
                let mut o = Vec::with_capacity(80);
                let total = (2u64 << 30) / 4096;
                for v in [total, total / 2, total / 2, 1000, 1_000_000] {
                    o.extend_from_slice(&v.to_le_bytes());
                }
                for v in [4096u32, 255, 4096, 0, 0, 0, 0, 0, 0, 0] {
                    o.extend_from_slice(&v.to_le_bytes());
                }
                Ok(o)
            }
            18 | 29 | 20 | 25 | 30 | 34 => Ok(vec![]),
            22 => Err(ENODATA),
            23 => {
                if u32at(b, 0) == 0 {
                    Ok(vec![0u8; 8])
                } else {
                    Ok(vec![])
                }
            }
            21 | 24 | 43 => Err(ENOTSUP),
            28 => {
                let off = u64at(b, 8) as usize;
                let size = u32at(b, 16) as usize;
                let s = self.inner.lock().unwrap();
                let Some(d) = s.nodes.get(&ino) else { return Some(Err(ENOENT)) };
                let mut kids: Vec<(String, u64, bool)> = s
                    .by_path
                    .iter()
                    .filter(|(k, _)| !k.is_empty() && parent_of(k) == d.path)
                    .map(|(k, &i)| (k.rsplit('/').next().unwrap_or(k).to_string(), i, s.nodes[&i].dir))
                    .collect();
                kids.sort();
                let mut all = vec![(".".to_string(), ino, true), ("..".to_string(), 1, true)];
                all.extend(kids);
                let mut o = Vec::new();
                for (idx, (name, ci, dir)) in all.iter().enumerate().skip(off) {
                    let rec = 24 + name.len();
                    let padded = rec.div_ceil(8) * 8;
                    if o.len() + padded > size {
                        break;
                    }
                    o.extend_from_slice(&ci.to_le_bytes());
                    o.extend_from_slice(&((idx + 1) as u64).to_le_bytes());
                    o.extend_from_slice(&(name.len() as u32).to_le_bytes());
                    o.extend_from_slice(&(if *dir { 4u32 } else { 8u32 }).to_le_bytes());
                    o.extend_from_slice(name.as_bytes());
                    o.resize(o.len() + padded - rec, 0);
                }
                Ok(o)
            }
            35 => {
                let flags = u32at(b, 0) as i32;
                let mode = u32at(b, 4) & 0o7777 & !u32at(b, 8);
                let (name, _) = cstr(&b[16..]);
                let Some(par) = self.node(ino) else { return Some(Err(ENOENT)) };
                let cp = join(&par.path, name);
                *detail = format!("{name} flags={flags:#o} mode={mode:o}");
                if let Err(e) = self.call(cl, &format!("/write?path={}&offset=0&mtime={mtime}&create=1&mode={mode}&op={}", pct_encode(&cp), self.op(unique, 0)), &[], ctx) {
                    if e != EEXIST || flags & libc::O_EXCL != 0 {
                        return Some(Err(e));
                    }
                }
                let mut s = self.inner.lock().unwrap();
                let ci = match s.by_path.get(&cp) {
                    Some(&i) => i,
                    None => s.assign(&cp, false, mode, 0, mtime),
                };
                let _ = File::create(self.cache_file(ci));
                let mut o = entry(ci, &s.nodes[&ci]);
                o.extend_from_slice(&open_out(ci << 8));
                Ok(o)
            }
            38 => Ok(vec![]),
            _ => Err(ENOSYS),
        })
    }
}

fn mmap_journal(path: &PathBuf, slots: usize) -> std::io::Result<*mut u8> {
    let f = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path)?;
    f.set_len((slots * SLOT) as u64)?;
    use std::os::fd::AsRawFd;
    // SAFETY: a shared read/write mapping of a file we just sized; it lives for the whole process.
    let p = unsafe {
        libc::mmap(std::ptr::null_mut(), slots * SLOT, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, f.as_raw_fd(), 0)
    };
    if p == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error());
    }
    std::mem::forget(f);
    Ok(p.cast())
}

struct SendPtr(*mut u8);
unsafe impl Send for SendPtr {}
unsafe impl Sync for SendPtr {}

fn write_reply(fd: RawFd, r: &[u8]) -> i32 {
    // SAFETY: write(2) of an owned buffer.
    let n = unsafe { libc::write(fd, r.as_ptr().cast(), r.len()) };
    if n < 0 { std::io::Error::last_os_error().raw_os_error().unwrap_or(0) } else { 0 }
}

pub fn serve(o: Opts) -> std::io::Result<()> {
    fs::create_dir_all(&o.state)?;
    let cache = o.state.join("cache");
    fs::create_dir_all(&cache)?;
    let boot_f = o.state.join("boot");
    let boot = match fs::read_to_string(&boot_f) {
        Ok(s) => u64::from_str_radix(s.trim(), 16).unwrap_or(1),
        Err(_) => {
            let mut b = [0u8; 8];
            File::open("/dev/urandom")?.read_exact(&mut b)?;
            let v = u64::from_le_bytes(b);
            fs::write(&boot_f, format!("{v:016x}"))?;
            v
        }
    };
    let t_start = now_us();
    let (fd, fresh) = match keeper::get(&o.keeper)? {
        Some(fd) => (fd, false),
        None => {
            let p = std::ffi::CString::new("/dev/fuse").unwrap();
            // SAFETY: open(2).
            let fd = unsafe { libc::open(p.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let data = format!("fd={fd},rootmode=40755,user_id=0,group_id=0,default_permissions,allow_other");
            let src = std::ffi::CString::new("supervisor").unwrap();
            let tgt = std::ffi::CString::new(o.mount.clone()).unwrap();
            let ty = std::ffi::CString::new("fuse").unwrap();
            let d = std::ffi::CString::new(data.clone()).unwrap();
            // SAFETY: mount(2) with NUL-terminated strings.
            let r = unsafe {
                libc::mount(src.as_ptr(), tgt.as_ptr(), ty.as_ptr(), libc::MS_NOSUID | libc::MS_NODEV, d.as_ptr().cast())
            };
            if r < 0 {
                let e = std::io::Error::last_os_error();
                eprintln!("mount {} ({data}) failed: {e}", o.mount);
                return Err(e);
            }
            eprintln!("mounted {} ({data})", o.mount);
            keeper::put(&o.keeper, fd)?;
            (fd, true)
        }
    };
    // node ids
    let nl = o.state.join("nodes.log");
    let mut by_path = HashMap::new();
    let mut nodes = HashMap::new();
    let mut next = 2;
    nodes.insert(1, Node { path: String::new(), dir: true, mode: 0o700, size: 0, mtime: now_ns(), ctime: now_ns(), gone: false });
    by_path.insert(String::new(), 1u64);
    if let Ok(f) = File::open(&nl) {
        for l in BufReader::new(f).lines().map_while(Result::ok) {
            let mut it = l.splitn(3, ' ');
            let (k, i, p) = (it.next().unwrap_or(""), it.next().unwrap_or("0").parse::<u64>().unwrap_or(0), it.next().unwrap_or(""));
            match k {
                "a" | "r" => {
                    if let Some(n) = nodes.get(&i).cloned() {
                        by_path.remove(&n.path);
                    }
                    nodes.insert(i, Node { path: p.to_string(), dir: false, mode: 0o600, size: 0, mtime: 0, ctime: 0, gone: true });
                    by_path.insert(p.to_string(), i);
                    next = next.max(i + 1);
                }
                "d" => {
                    if let Some(n) = nodes.get_mut(&i) {
                        n.gone = true;
                        let p = n.path.clone();
                        if by_path.get(&p) == Some(&i) {
                            by_path.remove(&p);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let nlog = OpenOptions::new().create(true).append(true).open(&nl)?;
    let mut inner = Inner { nodes, by_path, next, nlog };
    // tree from magpie
    let mut cl = Client::new(&o.magpie);
    let tree = loop {
        match cl.call("GET", "/tree", &[], &mut || {}) {
            Ok((200, b)) => break String::from_utf8_lossy(&b).into_owned(),
            _ => thread::sleep(Duration::from_millis(200)),
        }
    };
    let mut seen = std::collections::HashSet::new();
    seen.insert(1u64);
    for l in tree.lines() {
        let mut it = l.splitn(5, ' ');
        let k = it.next().unwrap_or("f");
        let mode = it.next().unwrap_or("0").parse().unwrap_or(0o600);
        let size = it.next().unwrap_or("0").parse().unwrap_or(0);
        let mtime = it.next().unwrap_or("0").parse().unwrap_or(0);
        let p = it.next().unwrap_or("");
        let i = inner.assign(p, k == "d", mode, size, mtime);
        seen.insert(i);
    }
    let stale: Vec<String> = inner.by_path.iter().filter(|(_, i)| !seen.contains(i)).map(|(p, _)| p.clone()).collect();
    for p in stale {
        inner.by_path.remove(&p);
    }
    for (i, n) in &mut inner.nodes {
        if !seen.contains(i) {
            n.gone = true;
        }
    }
    let mut kept = 0;
    for e in fs::read_dir(&cache)?.flatten() {
        let i: u64 = e.file_name().to_string_lossy().parse().unwrap_or(0);
        let ok = inner.nodes.get(&i).is_some_and(|n| !n.gone && e.metadata().is_ok_and(|m| m.len() == n.size));
        if ok {
            kept += 1;
        } else {
            let _ = fs::remove_file(e.path());
        }
    }
    let fs_ = Arc::new(Fs {
        inner: Mutex::new(inner),
        boot,
        cache,
        oplog: Mutex::new(OpenOptions::new().create(true).append(true).open(o.state.join("ops.tsv"))?),
        die: o.die,
        fd,
        magpie: o.magpie.clone(),
    });
    let journal = mmap_journal(&o.journal, o.workers)?;
    // replay every occupied slot, in `unique` order
    let mut pending = Vec::new();
    for s in 0..o.workers {
        // SAFETY: inside the mapping.
        let slot = unsafe { std::slice::from_raw_parts_mut(journal.add(s * SLOT), SLOT) };
        let len = u32at(slot, 0);
        if len != 0 {
            pending.push((u64at(slot, 8), s));
        }
    }
    pending.sort();
    for (unique, s) in &pending {
        // SAFETY: inside the mapping.
        let slot = unsafe { std::slice::from_raw_parts_mut(journal.add(s * SLOT), SLOT) };
        let opcode = u32at(slot, 4);
        let r = fs_.handle(&mut cl, slot, false);
        let werr = r.as_ref().map_or(0, |r| write_reply(fd, r));
        eprintln!("replayed slot {s} unique {unique} {} -> reply write errno {werr}", opname(opcode));
        slot[0..4].copy_from_slice(&0u32.to_le_bytes());
    }
    eprintln!(
        "supervisor up: fresh_mount={fresh} boot={boot:016x} replayed={} cache_kept={kept} start_ms={}",
        pending.len(),
        (now_us() - t_start) / 1000
    );
    let mut hs = Vec::new();
    for s in 0..o.workers {
        let f = fs_.clone();
        let p = SendPtr(journal);
        hs.push(thread::spawn(move || {
            let p = p;
            // SAFETY: each worker owns slot `s` of the mapping.
            let slot = unsafe { std::slice::from_raw_parts_mut(p.0.add(s * SLOT), SLOT) };
            let mut cl = Client::new(&f.magpie);
            loop {
                slot[0..4].copy_from_slice(&0u32.to_le_bytes());
                // SAFETY: read(2) straight into the mapped slot.
                let n = unsafe { libc::read(f.fd, slot.as_mut_ptr().cast(), SLOT) };
                if n < 0 {
                    let e = std::io::Error::last_os_error();
                    match e.raw_os_error() {
                        Some(libc::EINTR | libc::EAGAIN) => continue,
                        _ => {
                            eprintln!("worker {s}: read: {e}");
                            return;
                        }
                    }
                }
                let opcode = u32at(slot, 4);
                let marked = f.die.as_ref().is_some_and(|d| {
                    d.opcode == opcode && d.seen.fetch_add(1, Ordering::SeqCst) + 1 == d.nth
                });
                f.maybe_die("read", &Ctx { opcode, marked });
                if let Some(r) = f.handle(&mut cl, slot, marked) {
                    let e = write_reply(f.fd, &r);
                    if e != 0 && e != libc::ENOENT {
                        eprintln!("worker {s}: reply write errno {e}");
                    }
                }
                f.maybe_die("replied", &Ctx { opcode, marked });
            }
        }));
    }
    for h in hs {
        let _ = h.join();
    }
    Ok(())
}

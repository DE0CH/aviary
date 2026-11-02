//! Fake magpie: the conversation's `~/.claude` files behind HTTP, with idempotent operation ids, latency injection
//! (`<ctl>/latency_ms`, applied to every mutating call) and a hold (`<ctl>/hold` exists: mutating calls wait).

use crate::http::{self, Reply, Request};
use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone)]
struct Entry {
    dir: bool,
    mode: u32,
    size: u64,
    mtime: u64,
}

struct St {
    files: BTreeMap<String, Entry>,
    ops: HashMap<String, (u16, Vec<u8>)>,
    log: File,
    calls: u64,
}

pub fn now_us() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_micros() as u64)
}

fn parent(p: &str) -> &str {
    p.rsplit_once('/').map_or("", |(a, _)| a)
}

pub fn run(listen: &str, data: PathBuf, ctl: PathBuf) -> std::io::Result<()> {
    fs::create_dir_all(data.join("d"))?;
    fs::create_dir_all(&ctl)?;
    let log = OpenOptions::new().create(true).append(true).open(data.join("calls.log"))?;
    let mut files = BTreeMap::new();
    files.insert(String::new(), Entry { dir: true, mode: 0o700, size: 0, mtime: 0 });
    // a pre-seeded store (a resumed conversation): every file under d/
    seed(&data.join("d"), "", &mut files);
    let st = Mutex::new(St { files, ops: HashMap::new(), log, calls: 0 });
    let st: &'static Mutex<St> = Box::leak(Box::new(st));
    let data: &'static PathBuf = Box::leak(Box::new(data));
    let ctl: &'static PathBuf = Box::leak(Box::new(ctl));
    eprintln!("fake magpie on {listen}");
    http::serve(listen, move |req, _| handle(st, data, ctl, req))
}

fn seed(dir: &std::path::Path, rel: &str, files: &mut BTreeMap<String, Entry>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let p = if rel.is_empty() { name } else { format!("{rel}/{name}") };
        let Ok(md) = e.metadata() else { continue };
        let mtime = md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as u64);
        if md.is_dir() {
            files.insert(p.clone(), Entry { dir: true, mode: 0o700, size: 0, mtime });
            seed(&e.path(), &p, files);
        } else {
            files.insert(p, Entry { dir: false, mode: 0o600, size: md.len(), mtime });
        }
    }
}

fn wait_ctl(ctl: &std::path::Path) -> u64 {
    let t = Instant::now();
    while ctl.join("hold").exists() {
        thread::sleep(Duration::from_millis(20));
    }
    if let Ok(s) = fs::read_to_string(ctl.join("latency_ms")) {
        if let Ok(ms) = s.trim().parse::<u64>() {
            if ms > 0 {
                thread::sleep(Duration::from_millis(ms));
            }
        }
    }
    t.elapsed().as_micros() as u64
}

fn r(code: u16, b: impl Into<Vec<u8>>) -> Reply {
    Reply::Full(code, "text/plain", b.into())
}

fn handle(st: &Mutex<St>, data: &PathBuf, ctl: &PathBuf, req: Request) -> Reply {
    let t0 = now_us();
    let path = req.q("path").to_string();
    let fp = |p: &str| data.join("d").join(p);
    if req.method == "GET" {
        let s = st.lock().unwrap();
        return match req.path.as_str() {
            "/tree" => {
                let mut o = String::new();
                for (p, e) in &s.files {
                    if p.is_empty() {
                        continue;
                    }
                    o.push_str(&format!("{} {} {} {} {}\n", if e.dir { 'd' } else { 'f' }, e.mode, e.size, e.mtime, p));
                }
                r(200, o)
            }
            "/file" => {
                let Some(e) = s.files.get(&path) else { return r(404, "not_found") };
                if e.dir {
                    return r(400, "bad_request");
                }
                let off = req.qu64("offset");
                let len = req.q("length").parse::<u64>().unwrap_or(e.size);
                let n = len.min(e.size.saturating_sub(off));
                let mut b = vec![0; n as usize];
                if n > 0 {
                    if let Ok(f) = File::open(fp(&path)) {
                        let _ = f.read_exact_at(&mut b, off);
                    }
                }
                r(200, b)
            }
            "/stats" => r(200, format!("calls {}\n", s.calls)),
            _ => r(404, "no route"),
        };
    }
    let waited = wait_ctl(ctl);
    let op = req.q("op").to_string();
    let mut s = st.lock().unwrap();
    s.calls += 1;
    if !op.is_empty() {
        if let Some((c, b)) = s.ops.get(&op).cloned() {
            let line = format!("{t0} {} {} op={op} replayed={c}\n", req.path, path);
            let _ = s.log.write_all(line.as_bytes());
            return Reply::Full(c, "text/plain", b);
        }
    }
    let mtime = req.qu64("mtime");
    let (code, body): (u16, Vec<u8>) = (|| -> (u16, Vec<u8>) {
        match req.path.as_str() {
            "/write" => {
                let create = req.q("create") == "1";
                let off = req.qu64("offset");
                match s.files.get(&path).cloned() {
                    None if create => {
                        match s.files.get(parent(&path)) {
                            Some(p) if p.dir => {}
                            _ => return (404, b"not_found".to_vec()),
                        }
                        if File::create(fp(&path)).is_err() {
                            return (500, b"io".to_vec());
                        }
                        let mode = req.q("mode").parse().unwrap_or(0o600);
                        s.files.insert(path.clone(), Entry { dir: false, mode, size: 0, mtime });
                        (200, b"0".to_vec())
                    }
                    None => (404, b"not_found".to_vec()),
                    Some(e) if e.dir => (400, b"bad_request".to_vec()),
                    Some(_) if create && req.body.is_empty() => (409, b"exists".to_vec()),
                    Some(e) => {
                        if off > e.size {
                            return (409, b"offset_beyond_eof".to_vec());
                        }
                        let Ok(f) = OpenOptions::new().write(true).open(fp(&path)) else { return (500, b"io".to_vec()) };
                        if f.write_all_at(&req.body, off).is_err() {
                            return (500, b"io".to_vec());
                        }
                        let ns = e.size.max(off + req.body.len() as u64);
                        let en = s.files.get_mut(&path).unwrap();
                        en.size = ns;
                        en.mtime = mtime;
                        (200, ns.to_string().into_bytes())
                    }
                }
            }
            "/truncate" => {
                let size = req.qu64("size");
                let Some(e) = s.files.get_mut(&path) else { return (404, b"not_found".to_vec()) };
                if e.dir {
                    return (400, b"bad_request".to_vec());
                }
                if let Ok(f) = OpenOptions::new().write(true).open(fp(&path)) {
                    let _ = f.set_len(size);
                }
                e.size = size;
                e.mtime = mtime;
                (200, b"ok".to_vec())
            }
            "/setattr" => {
                let Some(e) = s.files.get_mut(&path) else { return (404, b"not_found".to_vec()) };
                if mtime != 0 {
                    e.mtime = mtime;
                }
                if let Ok(m) = req.q("mode").parse::<u32>() {
                    e.mode = m;
                }
                (200, b"ok".to_vec())
            }
            "/mkdir" => {
                if s.files.contains_key(&path) {
                    return (409, b"exists".to_vec());
                }
                match s.files.get(parent(&path)) {
                    Some(p) if p.dir => {}
                    _ => return (404, b"not_found".to_vec()),
                }
                let _ = fs::create_dir_all(fp(&path));
                let mode = req.q("mode").parse().unwrap_or(0o700);
                s.files.insert(path.clone(), Entry { dir: true, mode, size: 0, mtime });
                (200, b"ok".to_vec())
            }
            "/rmdir" => {
                match s.files.get(&path) {
                    Some(e) if e.dir => {}
                    Some(_) => return (400, b"bad_request".to_vec()),
                    None => return (404, b"not_found".to_vec()),
                }
                let pre = format!("{path}/");
                if s.files.range(pre.clone()..).next().is_some_and(|(k, _)| k.starts_with(&pre)) {
                    return (409, b"not_empty".to_vec());
                }
                let _ = fs::remove_dir(fp(&path));
                s.files.remove(&path);
                (200, b"ok".to_vec())
            }
            "/unlink" => match s.files.get(&path) {
                Some(e) if !e.dir => {
                    let _ = fs::remove_file(fp(&path));
                    s.files.remove(&path);
                    (200, b"ok".to_vec())
                }
                Some(_) => (400, b"bad_request".to_vec()),
                None => (404, b"not_found".to_vec()),
            },
            "/rename" => {
                let to = req.q("to").to_string();
                let Some(e) = s.files.get(&path).cloned() else { return (404, b"not_found".to_vec()) };
                match s.files.get(parent(&to)) {
                    Some(p) if p.dir => {}
                    _ => return (404, b"not_found".to_vec()),
                }
                if let Some(t) = s.files.get(&to) {
                    if t.dir != e.dir {
                        return (400, b"bad_request".to_vec());
                    }
                    let pre = format!("{to}/");
                    if t.dir && s.files.range(pre.clone()..).next().is_some_and(|(k, _)| k.starts_with(&pre)) {
                        return (409, b"not_empty".to_vec());
                    }
                }
                if fs::rename(fp(&path), fp(&to)).is_err() {
                    return (500, b"io".to_vec());
                }
                let pre = format!("{path}/");
                let moved: Vec<(String, Entry)> =
                    s.files.range(pre.clone()..).take_while(|(k, _)| k.starts_with(&pre)).map(|(k, v)| (k.clone(), v.clone())).collect();
                s.files.remove(&to);
                s.files.remove(&path);
                s.files.insert(to.clone(), e);
                for (k, v) in moved {
                    s.files.remove(&k);
                    s.files.insert(format!("{to}/{}", &k[pre.len()..]), v);
                }
                (200, b"ok".to_vec())
            }
            _ => (404, b"no route".to_vec()),
        }
    })();
    if !op.is_empty() {
        s.ops.insert(op.clone(), (code, body.clone()));
    }
    let line = format!(
        "{t0} {} {} off={} len={} op={op} code={code} waited_us={waited} took_us={}\n",
        req.path,
        path,
        req.q("offset"),
        req.body.len(),
        now_us() - t0
    );
    let _ = s.log.write_all(line.as_bytes());
    Reply::Full(code, "text/plain", body)
}

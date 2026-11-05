//! `holds.js` against fake `CloudCLI` children answering the switch module's busy route (cutover 7.12 item 5's script half; the exec through the Kubernetes API
//! and the gate are the hand-over's): every child of the old router is asked with its owner, a child that answers
//! 500 or not at all within 5 s is `ok: false`, a process without the router's child environment is not asked, and
//! a child's own processes (which inherit its environment) do not ask it twice, and a list entry without a string
//! `sessionId` (a changed route shape) is `ok: false`, never "runs nothing". Of the parked questions (`asking`,
//! DECISIONS 17) only an answered one makes its conversation running.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use magpie_import::holds;

const FAKE: &str = r"
const http = require('http');
const mode = process.env.FAKE_MODE, owner = process.env.JARVIS_CC_OWNER;
http.createServer((req, res) => {
  if (mode === 'hang') return;
  if (mode === '500') { res.statusCode = 500; return res.end('no'); }
  if (mode === 'noid') {
    res.setHeader('content-type', 'application/json');
    return res.end(JSON.stringify({ running: [{ id: 'x' }], processes: [], preparing: [], pending: [] }));
  }
  if (mode === 'asking' || mode === 'badasking') {
    // DECISIONS 17: parked questions; only the answered one waits for its turn (running)
    res.setHeader('content-type', 'application/json');
    const asking = mode === 'badasking' ? [{ requestId: 'r' }] : [
      { sessionId: '33333333-3333-3333-3333-333333333333', requestId: 'r1', toolName: 'AskUserQuestion',
        toolUseId: 'toolu_1', askedAt: '2026-11-05T07:00:00.000Z', answered: false },
      { sessionId: '44444444-4444-4444-4444-444444444444', requestId: 'r2', toolName: 'AskUserQuestion',
        toolUseId: 'toolu_2', askedAt: '2026-11-05T07:00:00.000Z', answered: true }];
    return res.end(JSON.stringify({ running: [], processes: [], preparing: [], pending: [], asking }));
  }
  if (mode === 'nolist') {
    res.setHeader('content-type', 'application/json');
    return res.end(JSON.stringify({ running: [], processes: [] }));
  }
  if (req.url !== '/api/jarvis/switch/busy' || req.headers['x-jarvis-user'] !== owner) {
    res.statusCode = 404; return res.end();
  }
  res.setHeader('content-type', 'application/json');
  // the ids spread over the four lists, one repeated: the script answers each once
  const ids = JSON.parse(process.env.FAKE_SESSIONS);
  const lists = { running: [], processes: [], preparing: [], pending: [] };
  const keys = Object.keys(lists);
  ids.forEach((s, i) => lists[keys[i % 4]].push(s));
  if (ids.length) lists.processes.push(ids[0]);
  res.end(JSON.stringify(lists));
}).listen(Number(process.env.SERVER_PORT || 0), '127.0.0.1', () => console.log('ready'));
// a process of the child (inherits its environment), which ends when the child does
if (process.env.FAKE_FORK) require('child_process').spawn(process.execPath,
  ['-e', 'const p = Number(process.env.FAKE_PARENT); setInterval(() => { try { process.kill(p, 0); } catch (e) { process.exit(0); } }, 200);'],
  { stdio: 'ignore', env: { ...process.env, FAKE_PARENT: String(process.pid) } });
";

struct Kids(Vec<Child>);

impl Drop for Kids {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn(env: &[(&str, String)]) -> Child {
    let mut c = Command::new("node");
    c.arg("-e").arg(FAKE).stdout(Stdio::piped()).stderr(Stdio::inherit());
    for (k, v) in env {
        c.env(k, v);
    }
    let mut child = c.spawn().expect("node is on PATH (ci.yml's runners have it)");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line.trim(), "ready");
    child
}

#[test]
fn holds_js_asks_every_child_once_and_fails_closed() {
    let (pa, pb, pc, pd, pe, pf) = (port(), port(), port(), port(), port(), port());
    let (pg, ph) = (port(), port());
    let mut kids = Kids(Vec::new());
    kids.0.push(spawn(&[
        ("DATABASE_SCHEMA", "cc_ua".into()),
        ("SERVER_PORT", pa.to_string()),
        ("JARVIS_CC_OWNER", "a@example.com".into()),
        // running and processes (the first id in both): each answered once
        (
            "FAKE_SESSIONS",
            "[\"11111111-1111-1111-1111-111111111111\",\"22222222-2222-2222-2222-222222222222\"]".into(),
        ),
        ("FAKE_FORK", "1".into()),
    ]));
    kids.0.push(spawn(&[
        ("DATABASE_SCHEMA", "cc_ub".into()),
        ("SERVER_PORT", pb.to_string()),
        ("JARVIS_CC_OWNER", "b@example.com".into()),
        ("FAKE_MODE", "500".into()),
    ]));
    kids.0.push(spawn(&[
        ("DATABASE_SCHEMA", "cc_uc".into()),
        ("SERVER_PORT", pc.to_string()),
        ("JARVIS_CC_OWNER", "c@example.com".into()),
        ("FAKE_MODE", "hang".into()),
    ]));
    kids.0.push(spawn(&[
        ("DATABASE_SCHEMA", "cc_ue".into()),
        ("SERVER_PORT", pe.to_string()),
        ("JARVIS_CC_OWNER", "e@example.com".into()),
        ("FAKE_MODE", "noid".into()),
    ]));
    // a list missing (an image without the A1 route's shape): busy, never "idle"
    kids.0.push(spawn(&[
        ("DATABASE_SCHEMA", "cc_uf".into()),
        ("SERVER_PORT", pf.to_string()),
        ("JARVIS_CC_OWNER", "f@example.com".into()),
        ("FAKE_MODE", "nolist".into()),
    ]));
    kids.0.push(spawn(&[
        ("DATABASE_SCHEMA", "cc_ug".into()),
        ("SERVER_PORT", pg.to_string()),
        ("JARVIS_CC_OWNER", "g@example.com".into()),
        ("FAKE_MODE", "asking".into()),
    ]));
    kids.0.push(spawn(&[
        ("DATABASE_SCHEMA", "cc_uh".into()),
        ("SERVER_PORT", ph.to_string()),
        ("JARVIS_CC_OWNER", "h@example.com".into()),
        ("FAKE_MODE", "badasking".into()),
    ]));
    // not a router child: no owner
    kids.0.push(spawn(&[
        ("DATABASE_SCHEMA", "cc_ud".into()),
        ("SERVER_PORT", pd.to_string()),
        ("FAKE_SESSIONS", "[]".into()),
    ]));
    std::thread::sleep(Duration::from_millis(300));
    let started = std::time::Instant::now();
    let out = Command::new("timeout")
        .args(["-k", "5", "30", "node", "-e", holds::SCRIPT])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "holds.js failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "a silent child holds the read for 5 s only"
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let ours: Vec<String> = kids.0.iter().map(|k| format!("pid {}:", k.id())).collect();
    // other users' processes on this machine (an unprivileged runner) cannot be read: the script reports them, and
    // in a ui pod (one user) that makes the read incomplete; here only ours count
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.contains("\"incomplete\"") || ours.iter().any(|o| l.contains(o.as_str())))
        .collect();
    let lines = lines.join("\n");
    let mut children = holds::parse(&lines).unwrap();
    // other tests' processes on this machine are not ours
    children.retain(|c| [pa, pb, pc, pd, pe, pf, pg, ph].contains(&c.port));
    children.sort_by_key(|c| c.schema.clone());
    let got: Vec<(String, bool, Vec<String>)> = children.into_iter().map(|c| (c.schema, c.ok, c.sessions)).collect();
    assert_eq!(
        got,
        [
            (
                "cc_ua".to_owned(),
                true,
                vec![
                    "11111111-1111-1111-1111-111111111111".to_owned(),
                    "22222222-2222-2222-2222-222222222222".to_owned()
                ]
            ),
            ("cc_ub".to_owned(), false, vec![]),
            ("cc_uc".to_owned(), false, vec![]),
            ("cc_ue".to_owned(), false, vec![]),
            ("cc_uf".to_owned(), false, vec![]),
            // an unanswered parked question holds nothing (it is carried over); an answered one is running
            (
                "cc_ug".to_owned(),
                true,
                vec!["44444444-4444-4444-4444-444444444444".to_owned()]
            ),
            ("cc_uh".to_owned(), false, vec![]),
        ]
    );
}

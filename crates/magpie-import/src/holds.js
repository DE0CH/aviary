// magpie import's running-set read (cutover 6.3, DESIGN 9.4 conditions 2 and 3), run in a ui pod's container `ui` as
// `node -e <this>`: every CloudCLI child of the old router (a process whose environment has DATABASE_SCHEMA,
// SERVER_PORT and JARVIS_CC_OWNER, jarvis/router.mjs) is asked what keeps it busy, the call the router's switch module
// makes before a restart: GET http://127.0.0.1:<SERVER_PORT>/api/jarvis/switch/busy with x-jarvis-user:
// <JARVIS_CC_OWNER> (claudecodeui jarvis-switch.routes.ts, in the A1 image) -> {running, processes, preparing,
// pending, asking}: the first four lists of app session ids (chat runs and tracked background work, Claude Code
// processes held open, sandboxes being made, Paseo messages not yet completed), `asking` the questions waiting for the
// person after their turn let the sandbox go (DECISIONS 17; claudecodeui fd1b3ed5: {sessionId, requestId, toolName,
// toolUseId, askedAt, answered}). An unanswered question holds nothing (the importer carries it over, questions.rs); an
// answered one waits for its turn to start, so its session counts as running. One JSON line per child: {"schema",
// "port", "ok", "sessions": [every id of the four lists and of the answered questions]}. An image without `asking`
// (before fd1b3ed5) has no parked questions; an `asking` of another shape is ok: false. A child that does not answer 200 with the four lists of strings within 5 s
// is ok: false (the importer then treats every conversation of that schema as busy), so an image without the route
// (before A1) releases nothing.
// A process whose environment cannot be read for another reason than having exited (EACCES, hidepid) may be a child:
// the script prints {"incomplete": <why>}, which makes the pod's read unusable. Processes the child started inherit
// its environment; each (schema, port) is asked once.
'use strict';
const fs = require('fs');
const http = require('http');
const WANT = ['DATABASE_SCHEMA', 'SERVER_PORT', 'JARVIS_CC_OWNER'];
const seen = new Map();
for (const pid of fs.readdirSync('/proc')) {
  if (!/^[0-9]+$/.test(pid)) continue;
  let raw;
  try {
    raw = fs.readFileSync('/proc/' + pid + '/environ');
  } catch (e) {
    // gone since the listing (ENOENT, ESRCH): not a child any more; anything else: this read cannot be trusted
    if (e && (e.code === 'ENOENT' || e.code === 'ESRCH')) continue;
    process.stdout.write(JSON.stringify({ incomplete: 'pid ' + pid + ': ' + ((e && e.code) || String(e)) }) + '\n');
    continue;
  }
  const env = {};
  for (const kv of raw.toString('utf8').split('\0')) {
    const i = kv.indexOf('=');
    if (i > 0) env[kv.slice(0, i)] = kv.slice(i + 1);
  }
  if (!WANT.every((k) => env[k])) continue;
  const port = Number(env.SERVER_PORT);
  if (!Number.isInteger(port) || port <= 0 || port > 65535) continue;
  const id = env.DATABASE_SCHEMA + ':' + port;
  if (!seen.has(id)) seen.set(id, { schema: env.DATABASE_SCHEMA, port, owner: env.JARVIS_CC_OWNER });
}
function ask(c) {
  return new Promise((resolve) => {
    let done = false;
    const finish = (ok, sessions) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      resolve({ schema: c.schema, port: c.port, ok, sessions });
    };
    const q = http.get({ host: '127.0.0.1', port: c.port, path: '/api/jarvis/switch/busy',
                         headers: { 'x-jarvis-user': c.owner } }, (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (d) => { if (body.length < (4 << 20)) body += d; });
      res.on('end', () => {
        if (res.statusCode !== 200) return finish(false, []);
        try {
          const j = JSON.parse(body);
          const lists = ['running', 'processes', 'preparing', 'pending'].map((k) => j && j[k]);
          // a changed route shape must never read as "idle"
          if (!lists.every((l) => Array.isArray(l) && l.every((e) => typeof e === 'string' && e))) return finish(false, []);
          const asking = j.asking === undefined ? [] : j.asking;
          if (!Array.isArray(asking) || !asking.every((q) => q && typeof q.sessionId === 'string' && q.sessionId
              && typeof q.answered === 'boolean')) return finish(false, []);
          const answered = asking.filter((q) => q.answered).map((q) => q.sessionId);
          finish(true, [...new Set(lists.flat().concat(answered))]);
        } catch (e) { finish(false, []); }
      });
      res.on('error', () => finish(false, []));
    });
    const timer = setTimeout(() => { q.destroy(); finish(false, []); }, 5000);
    q.on('error', () => finish(false, []));
  });
}
Promise.all([...seen.values()].map(ask)).then((rows) => {
  for (const r of rows) process.stdout.write(JSON.stringify(r) + '\n');
  process.exit(0);
});

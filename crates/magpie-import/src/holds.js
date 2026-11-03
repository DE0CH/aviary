// magpie import's running-set read (cutover 6.3), run in a ui pod's container `ui` as `node -e <this>`: every
// CloudCLI child of the old router (a process whose environment has DATABASE_SCHEMA, SERVER_PORT and JARVIS_CC_OWNER,
// jarvis/router.mjs) is asked for its running list, the call the router itself makes (runningChats()):
// GET http://127.0.0.1:<SERVER_PORT>/api/providers/sessions/running with x-jarvis-user: <JARVIS_CC_OWNER>. One JSON
// line per child: {"schema", "port", "ok", "sessions": [<app session id>…]}. A child that does not answer 200 with a
// session list within 5 s is ok: false (the importer then treats every conversation of that schema as running).
// Processes the child started inherit its environment; each (schema, port) is asked once.
'use strict';
const fs = require('fs');
const http = require('http');
const WANT = ['DATABASE_SCHEMA', 'SERVER_PORT', 'JARVIS_CC_OWNER'];
const seen = new Map();
for (const pid of fs.readdirSync('/proc')) {
  if (!/^[0-9]+$/.test(pid)) continue;
  let raw;
  try { raw = fs.readFileSync('/proc/' + pid + '/environ'); } catch (e) { continue; }
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
    const q = http.get({ host: '127.0.0.1', port: c.port, path: '/api/providers/sessions/running',
                         headers: { 'x-jarvis-user': c.owner } }, (res) => {
      let body = '';
      res.setEncoding('utf8');
      res.on('data', (d) => { if (body.length < (4 << 20)) body += d; });
      res.on('end', () => {
        if (res.statusCode !== 200) return finish(false, []);
        try {
          const j = JSON.parse(body);
          const s = (j && j.data && j.data.sessions) || (j && j.sessions);
          if (!Array.isArray(s)) return finish(false, []);
          finish(true, s.map((e) => (e && typeof e.sessionId === 'string' ? e.sessionId : null)).filter(Boolean));
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

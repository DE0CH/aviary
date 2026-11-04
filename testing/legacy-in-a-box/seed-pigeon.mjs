// A plain client of pigeon for the seeded defects (WP-54, run.sh seeds): a person's Paseo socket and the upload
// route, as the web app uses them, with an Access token signed by the given key. Prints each answer as a JSON line
// and exits 0 once the operation was answered (whatever the answer: the seeds read the counters, not this).
//   node seed-pigeon.mjs <pigeon url> <key pem> <kid> <iss> <aud> <email> <op> [args]
// ops:
//   fetch <conversation>                     fetch_agent_request (a legacy conversation's goes over the LU)
//   send <conversation> <text> [<path>]      send_agent_message_request, with one attachment of that path
//   create <text> [<path>]                   workspace.create.request with a first message (and that attachment)
//   upload <name>                            POST /api/assets/files with one small text file
import { createSign, randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";

const [base, keyFile, kid, iss, aud, email, op, ...rest] = process.argv.slice(2);
const now = Math.floor(Date.now() / 1000);
const b = (x) => Buffer.from(JSON.stringify(x)).toString("base64url");
const h = b({ alg: "RS256", kid, typ: "JWT" });
const p = b({ iss, aud: [aud], email, exp: now + 3 * 3600, iat: now, nbf: now - 5, type: "app", identity_nonce: randomUUID() });
const tok = `${h}.${p}.${createSign("RSA-SHA256").update(`${h}.${p}`).sign(readFileSync(keyFile)).toString("base64url")}`;
const auth = { "cf-access-jwt-assertion": tok, cookie: `CF_Authorization=${tok}` };
const out = (o) => console.log(JSON.stringify(o));
const done = (code = 0) => setTimeout(() => process.exit(code), 200);
setTimeout(() => { out({ error: "no answer in 120 s" }); process.exit(1); }, 120000);

if (op === "upload") {
  const fd = new FormData();
  fd.append("files", new Blob([`seed ${randomUUID()}\n`], { type: "text/plain" }), rest[0] || "seed.txt");
  const r = await fetch(`${base}/api/assets/files`, { method: "POST", headers: auth, body: fd });
  out({ op, status: r.status, body: (await r.text()).slice(0, 400) });
  done();
} else {
  const ws = new WebSocket(`${base.replace(/^http/, "ws")}/paseo-ws`, { headers: auth });
  const rid = randomUUID();
  const attach = (path) => (path ? [{ path, fileName: path.split("/").pop(), mimeType: "text/plain", size: 10 }] : undefined);
  let message;
  if (op === "fetch") message = { type: "fetch_agent_request", requestId: rid, agentId: rest[0] };
  else if (op === "send") message = { type: "send_agent_message_request", requestId: rid, agentId: rest[0], messageId: randomUUID(), text: rest[1] || "seed", attachments: attach(rest[2]) };
  else if (op === "create") message = { type: "workspace.create.request", requestId: rid, idempotencyKey: randomUUID(), agent: { provider: "claude", initialPrompt: rest[0] || "seed", attachments: attach(rest[1]) } };
  else { out({ error: `unknown op ${op}` }); process.exit(2); }
  ws.onopen = () => {
    ws.send(JSON.stringify({ type: "hello", protocolVersion: 1, clientType: "web", appVersion: "seed" }));
    setTimeout(() => ws.send(JSON.stringify({ type: "session", message })), 300);
  };
  ws.onmessage = (m) => {
    let f;
    try { f = JSON.parse(String(m.data)); } catch { return; }
    const msg = f.message || f;
    const t = msg.type || f.type;
    if (msg?.payload?.requestId === rid || msg?.requestId === rid || /rpc_error/.test(t || "")) {
      out({ op, type: t, payload: JSON.stringify(msg.payload ?? msg).slice(0, 600) });
      if (!/\.update$/.test(t || "")) { ws.close(); done(); }
    }
  };
  ws.onclose = (e) => { out({ op, closed: e.code, reason: e.reason }); done(); };
  ws.onerror = (e) => { out({ op, error: String(e?.message || e) }); done(); };
}

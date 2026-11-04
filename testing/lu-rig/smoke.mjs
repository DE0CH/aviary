// The rig's own check that the old stack runs a real turn (run.sh smoke): as staff@example.org through the ui's
// router (an Access token signed with the rig's key), create a session, send "[[slow 3]] hello" over CloudCLI's /ws,
// wait for its completion and read the history back. Usage: node smoke.mjs <ui url> <key pem>
import { createSign, randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";

const [ui, keyFile] = process.argv.slice(2);
const key = readFileSync(keyFile);
export function token(email, life = 3600) {
  const now = Math.floor(Date.now() / 1000);
  const b = (o) => Buffer.from(JSON.stringify(o)).toString("base64url");
  const h = b({ alg: "RS256", kid: "t1-a", typ: "JWT" });
  const p = b({ iss: "https://one.example.org", aud: ["aud-one"], email, exp: now + life, iat: now, nbf: now - 5, type: "app" });
  return `${h}.${p}.${createSign("RSA-SHA256").update(`${h}.${p}`).sign(key).toString("base64url")}`;
}
const email = "staff@example.org";
const H = { "cf-access-jwt-assertion": token(email), "content-type": "application/json" };
const j = async (r) => { const t = await r.text(); try { return JSON.parse(t); } catch { return t; } };
const created = await j(await fetch(`${ui}/api/providers/sessions`, { method: "POST", headers: H, body: JSON.stringify({ provider: "claude", projectPath: "/workspace" }) }));
const sessionId = created?.data?.sessionId;
if (!sessionId) { console.error("create failed", JSON.stringify(created)); process.exit(1); }
const ws = new WebSocket(`${ui.replace("http", "ws")}/ws`, { headers: { "cf-access-jwt-assertion": H["cf-access-jwt-assertion"] } });
await new Promise((r, e) => { ws.onopen = r; ws.onerror = e; });
const kinds = [];
const done = new Promise((resolve) => {
  ws.onmessage = (m) => { const f = JSON.parse(String(m.data)); kinds.push(f.kind); if (f.kind === "complete" || f.kind === "error") resolve(f); };
});
ws.send(JSON.stringify({ type: "chat.send", sessionId, content: `[[slow 3]] hello ${randomUUID()}`, options: { model: "claude-sonnet-5-5", permissionMode: "bypassPermissions" } }));
const end = await Promise.race([done, new Promise((r) => setTimeout(() => r(null), 120000))]);
ws.close();
await new Promise((r) => setTimeout(r, 3000));
const hist = await j(await fetch(`${ui}/api/providers/sessions/${sessionId}/messages`, { headers: H }));
const msgs = (hist?.data?.messages || []).map((m) => [m.kind, m.role, typeof m.content === "string" ? m.content.slice(0, 80) : ""]);
console.log(JSON.stringify({ sessionId, end: end?.kind ?? "timeout", frames: kinds.length, history: msgs }));
const ok = end?.kind === "complete" && msgs.some((m) => /done 3/.test(m[2]));
process.exit(ok ? 0 : 1);

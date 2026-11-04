// Seeded defects for the old router's zero counters (WP-54), as a raw client of an old ui pod's /paseo-ws as <email>:
//   inflight (switch_inflight_closes_total): a client that ignores the router's close frame and sends a request
//            after it (what the move's residual is: a Send pressed within the network delay of a close). Stay quiet
//            past the move's 5 s, move the person on that pod, and once the 1012 arrives write one request frame. The
//            router must count it (and not forward it).
//   opaque   (switch_opaque_sockets): a client that sends a fragmented message (a text frame without FIN), which the
//            router's reader cannot follow: the socket is opaque for its life. Held open for 90 s (the gauge counts
//            open sockets), then closed.
//   node seed-inflight.mjs <pod url> <switch token> <key pem> <iss> <aud> <email> [inflight|opaque]
import { createSign, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import net from "node:net";

const [pod, sw, keyFile, iss, aud, email, mode = "inflight"] = process.argv.slice(2);
const now = Math.floor(Date.now() / 1000);
const b = (x) => Buffer.from(JSON.stringify(x)).toString("base64url");
const h = b({ alg: "RS256", kid: "k1", typ: "JWT" });
const p = b({ iss, aud: [aud], email, exp: now + 3600, iat: now, nbf: now - 5 });
const tok = `${h}.${p}.${createSign("RSA-SHA256").update(`${h}.${p}`).sign(readFileSync(keyFile)).toString("base64url")}`;
const frame = (text) => {
  const pl = Buffer.from(text); const mask = randomBytes(4);
  const head = pl.length < 126 ? Buffer.from([0x81, 0x80 | pl.length]) : Buffer.from([0x81, 0x80 | 126, pl.length >> 8, pl.length & 255]);
  const out = Buffer.from(pl); for (let i = 0; i < out.length; i++) out[i] ^= mask[i & 3];
  return Buffer.concat([head, mask, out]);
};
const u = new URL(pod);
const s = net.connect(Number(u.port), u.hostname);
s.write(`GET /paseo-ws HTTP/1.1\r\nHost: x\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: ${randomBytes(16).toString("base64")}\r\ncf-access-jwt-assertion: ${tok}\r\n\r\n`);
let buf = Buffer.alloc(0), up = false, sent = false;
s.on("data", async (d) => {
  buf = Buffer.concat([buf, d]);
  if (!up) {
    const i = buf.indexOf("\r\n\r\n"); if (i < 0) return;
    if (!/^HTTP\/1.1 101/.test(buf.subarray(0, i).toString())) { console.error(buf.subarray(0, i).toString()); process.exit(1); }
    up = true; buf = buf.subarray(i + 4);
    if (mode === "opaque") {
      // the first fragment of a text message (FIN clear), masked as a client's must be; no continuation follows
      const f = frame(JSON.stringify({ type: "ping" }));
      f[0] = 0x01;
      s.write(f);
      console.log("sent a fragmented message");
      setTimeout(() => process.exit(0), 90000);
      return;
    }
    await new Promise((r) => setTimeout(r, 6000));
    const r = await fetch(`${pod}/__jarvis/move?person=${encodeURIComponent(email)}`, { method: "POST", headers: { "x-jarvis-switch-token": sw } });
    console.log("move", r.status, await r.text());
    return;
  }
  // the close frame (opcode 8, code 1012): the defect sends a request anyway
  if (!sent && buf.includes(Buffer.from([0x88]))) {
    sent = true;
    s.write(frame(JSON.stringify({ type: "session", message: { type: "send_agent_message_request", requestId: "seeded-after-close", agentId: "x", text: "late" } })));
    console.log("sent a request after the close frame");
    setTimeout(() => process.exit(0), 1500);
  }
});
s.on("error", (e) => { console.error(String(e)); process.exit(1); });
if (mode !== "opaque") setTimeout(() => { console.error("no close frame in 30 s"); process.exit(1); }, 30000);

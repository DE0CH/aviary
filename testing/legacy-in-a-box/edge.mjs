// The edge stand-in (WP-54, DESIGN 9.3): the browser's one origin. It serves the web app's files from --app (the
// persona rig's instrumented build) and passes every other request and upgrade on, unchanged, to the old tunnel
// (--old, the old ui's Service) until the flip, and to pigeon (--new) after it, except a stale location that keeps
// sending a share of the new connections to the old tunnel (DESIGN 9.9: its upgrade gets the 503, its HTTP is passed
// on to pigeon by the old router). Control on the same port, under /__edge/:
//   POST /__edge/flip?stale=<0..1>       the DNS change (stale: the share still routed to the old tunnel)
//   POST /__edge/defect?name=<name>      a seeded defect: `second-reconnect` closes each tab's first upgrade after
//                                        the flip 1.5 s after it opened (the rig must see a second reconnect)
//   POST /__edge/reset                   back to before the flip, no defect (between persona runs)
//   GET  /__edge/stats                   upgrades and requests per target, refused upgrades, the defect's closes
// Plain Node, no dependencies.
import { existsSync, readFileSync, statSync } from "node:fs";
import http from "node:http";
import net from "node:net";
import path from "node:path";

const arg = (k, d) => { const i = process.argv.indexOf(`--${k}`); return i > 0 ? process.argv[i + 1] : d; };
const PORT = Number(arg("port", "8443"));
const APP = arg("app", "");
const OLD = arg("old");
const NEW = arg("new");
const split = (hp) => { const [host, port] = hp.split(":"); return { host, port: Number(port) }; };
const targets = { old: split(OLD), new: split(NEW) };
let flipped = false, stale = 0, defect = null;
const stats = { upgrades: { old: 0, new: 0 }, requests: { old: 0, new: 0 }, refused: 0, defectCloses: 0, files: 0 };
const firstAfterFlip = new Set(); // tabs (by Access cookie) whose first upgrade after the flip was seen
const TYPES = { ".html": "text/html; charset=utf-8", ".js": "application/javascript", ".css": "text/css", ".json": "application/json",
  ".png": "image/png", ".svg": "image/svg+xml", ".ico": "image/x-icon", ".ttf": "font/ttf", ".woff2": "font/woff2", ".wasm": "application/wasm" };
const route = () => (!flipped ? "old" : Math.random() < stale ? "old" : "new");
const cookieOf = (req) => (/(?:^|;\s*)CF_Authorization=([^;]+)/.exec(req.headers.cookie || "") || [])[1] || req.headers["cf-access-jwt-assertion"] || "";

/** The app's own files (the instrumented build), the SPA's index for its routes. */
function file(req) {
  if (!APP || req.method !== "GET") return null;
  const p = decodeURIComponent((req.url || "/").split("?")[0]);
  if (p.startsWith("/api/") || p.startsWith("/__") || p === "/paseo-ws" || p === "/ws" || p.startsWith("/ops")) return null;
  const f = path.join(APP, path.normalize(p).replace(/^(\.\.[/\\])+/, ""));
  if (f.startsWith(APP) && existsSync(f) && statSync(f).isFile()) return f;
  // a client route of the app (expo-router): its index
  return path.extname(p) ? null : path.join(APP, "index.html");
}

const server = http.createServer((req, res) => {
  if (req.url.startsWith("/__edge/")) {
    const u = new URL(req.url, "http://x");
    res.setHeader("content-type", "application/json");
    if (u.pathname === "/__edge/flip" && req.method === "POST") { flipped = true; stale = Number(u.searchParams.get("stale") || 0); return res.end(JSON.stringify({ flipped, stale })); }
    if (u.pathname === "/__edge/reset" && req.method === "POST") { flipped = false; stale = 0; defect = null; firstAfterFlip.clear(); return res.end("{}"); }
    if (u.pathname === "/__edge/defect" && req.method === "POST") { defect = u.searchParams.get("name") || null; return res.end(JSON.stringify({ defect })); }
    return res.end(JSON.stringify({ flipped, stale, defect, ...stats }));
  }
  const f = file(req);
  if (f) {
    stats.files++;
    res.writeHead(200, { "content-type": TYPES[path.extname(f)] || "application/octet-stream", "cache-control": "no-store" });
    return res.end(readFileSync(f));
  }
  const side = route();
  stats.requests[side]++;
  const t = targets[side];
  const up = http.request({ host: t.host, port: t.port, method: req.method, path: req.url, headers: req.headers }, (ur) => {
    res.writeHead(ur.statusCode || 502, ur.headers);
    ur.pipe(res);
  });
  up.on("error", () => { if (!res.headersSent) { res.writeHead(502); res.end("edge: upstream error"); } else res.destroy(); });
  req.pipe(up);
});

// upgrades: the raw request out, then bytes both ways (the upstream's 503 or 101 reaches the client as it is)
server.on("upgrade", (req, sock, head) => {
  const side = route();
  stats.upgrades[side]++;
  const t = targets[side];
  const u = net.connect(t.port, t.host, () => {
    const lines = [`${req.method} ${req.url} HTTP/1.1`];
    for (let i = 0; i < req.rawHeaders.length; i += 2) lines.push(`${req.rawHeaders[i]}: ${req.rawHeaders[i + 1]}`);
    u.write(lines.join("\r\n") + "\r\n\r\n");
    if (head?.length) u.write(head);
    u.pipe(sock);
    sock.pipe(u);
  });
  const end = () => { sock.destroy(); u.destroy(); };
  u.on("error", end); sock.on("error", end); u.on("close", end); sock.on("close", end);
  // the seeded defect: a tab's first connection after the flip is dropped once more, soon after it opened
  const tab = cookieOf(req);
  if (defect === "second-reconnect" && flipped && side === "new" && tab && !firstAfterFlip.has(tab)) {
    firstAfterFlip.add(tab);
    setTimeout(() => { stats.defectCloses++; end(); }, 1500);
  }
});

server.listen(PORT, "0.0.0.0", () => console.log(JSON.stringify({ edge: PORT, app: APP || null, old: OLD, new: NEW })));

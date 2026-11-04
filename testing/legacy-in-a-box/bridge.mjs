// The bridges between the two clusters (DESIGN 15.5) in one process: each `--listen <port> --to <host:port>` is a
// plain TCP relay, as the Hetzner load balancers B1 (the new cluster -> the old ui) and B2 (the old ui's pass-on ->
// pigeon) are. GET :9999/__bridge says how many connections each relayed; POST :9999/__bridge/cut resets every open
// one (the bridge's target node killed mid-run). A seeded fault on one route's NEW connections (run.sh seeds):
// POST :9999/__bridge/fault?port=<listen port>&mode=<m>, m one of `503` (the old ui answers 503 to whatever comes,
// as a full pod does), `refuse` (the connection is reset at once: the target node gone) or `none` (relay again).
// Plain Node, no dependencies.
import http from "node:http";
import net from "node:net";

const args = process.argv.slice(2);
const routes = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--listen") routes.push({ port: Number(args[++i]), to: null, opened: 0, open: new Set(), fault: "none", faulted: 0 });
  else if (args[i] === "--to") { const [host, port] = args[++i].split(":"); routes.at(-1).to = { host, port: Number(port) }; }
}
for (const r of routes) {
  net.createServer((c) => {
    c.on("error", () => {});
    if (r.fault !== "none") {
      r.faulted++;
      if (r.fault === "503") {
        // the answer once the request's head is in (a client that waits for its own write to finish sees it)
        c.once("data", () => c.end("HTTP/1.1 503 Service Unavailable\r\ncontent-type: text/plain\r\ncontent-length: 5\r\nconnection: close\r\n\r\nbusy\n"));
      } else c.resetAndDestroy?.() ?? c.destroy();
      return;
    }
    const u = net.connect(r.to.port, r.to.host);
    r.opened++;
    const pair = { c, u };
    r.open.add(pair);
    c.on("error", () => {}); u.on("error", () => {});
    c.pipe(u); u.pipe(c);
    const end = () => { r.open.delete(pair); c.destroy(); u.destroy(); };
    c.on("close", end); u.on("close", end);
  }).listen(r.port, "0.0.0.0");
}
http.createServer((req, res) => {
  const u = new URL(req.url, "http://x");
  if (u.pathname === "/__bridge/fault" && req.method === "POST") {
    const r = routes.find((x) => x.port === Number(u.searchParams.get("port")));
    const mode = u.searchParams.get("mode") || "none";
    if (!r || !["503", "refuse", "none"].includes(mode)) { res.writeHead(400); return res.end("{}"); }
    r.fault = mode;
    return res.end(JSON.stringify({ port: r.port, fault: r.fault }));
  }
  if (req.url === "/__bridge/cut" && req.method === "POST") {
    let n = 0;
    for (const r of routes) for (const { c, u } of r.open) { c.resetAndDestroy?.() ?? c.destroy(); u.destroy(); n++; }
    return res.end(JSON.stringify({ cut: n }));
  }
  res.setHeader("content-type", "application/json");
  res.end(JSON.stringify(routes.map((r) => ({ port: r.port, to: `${r.to.host}:${r.to.port}`, opened: r.opened, open: r.open.size, fault: r.fault, faulted: r.faulted }))));
}).listen(9999, "0.0.0.0");
console.log(JSON.stringify({ routes: routes.map((r) => `${r.port}->${r.to.host}:${r.to.port}`) }));

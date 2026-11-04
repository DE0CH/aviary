// The bridges between the two clusters (DESIGN 15.5) in one process: each `--listen <port> --to <host:port>` is a
// plain TCP relay, as the Hetzner load balancers B1 (the new cluster -> the old ui) and B2 (the old ui's pass-on ->
// pigeon) are. GET :9999/__bridge says how many connections each relayed; POST :9999/__bridge/cut resets every open
// one (the bridge's target node killed mid-run). Plain Node, no dependencies.
import http from "node:http";
import net from "node:net";

const args = process.argv.slice(2);
const routes = [];
for (let i = 0; i < args.length; i++) {
  if (args[i] === "--listen") routes.push({ port: Number(args[++i]), to: null, opened: 0, open: new Set() });
  else if (args[i] === "--to") { const [host, port] = args[++i].split(":"); routes.at(-1).to = { host, port: Number(port) }; }
}
for (const r of routes) {
  net.createServer((c) => {
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
  if (req.url === "/__bridge/cut" && req.method === "POST") {
    let n = 0;
    for (const r of routes) for (const { c, u } of r.open) { c.resetAndDestroy?.() ?? c.destroy(); u.destroy(); n++; }
    return res.end(JSON.stringify({ cut: n }));
  }
  res.setHeader("content-type", "application/json");
  res.end(JSON.stringify(routes.map((r) => ({ port: r.port, to: `${r.to.host}:${r.to.port}`, opened: r.opened, open: r.open.size }))));
}).listen(9999, "0.0.0.0");
console.log(JSON.stringify({ routes: routes.map((r) => `${r.port}->${r.to.host}:${r.to.port}`) }));

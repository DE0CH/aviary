// The box's control for the persona drivers (run.sh personas starts it on the host): the runner's steps a driver
// needs at its own moments, each one run.sh subcommand. POST /<name>?conversation=<uuid> runs
// `run.sh <name> [<uuid>]` and answers its stdout (200) or its stderr (500). Names: gate (the runner's
// allow-handover), owner (legacy | owned), native-turns (the conversation's person turns in magpie).
import { execFile } from "node:child_process";
import http from "node:http";

const [script, port] = process.argv.slice(2);
const NAMES = new Set(["gate", "owner", "native-turns"]);
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
http.createServer((req, res) => {
  const u = new URL(req.url, "http://x");
  const name = u.pathname.slice(1);
  const c = u.searchParams.get("conversation");
  if (req.method !== "POST" || !NAMES.has(name) || (c && !UUID.test(c))) { res.writeHead(404); return res.end(); }
  execFile("bash", [script, name, ...(c ? [c] : [])], { timeout: 600000 }, (err, stdout, stderr) => {
    res.writeHead(err ? 500 : 200, { "content-type": "text/plain" });
    res.end(err ? `${stderr}`.slice(-2000) : stdout);
  });
}).listen(Number(port), "0.0.0.0", () => console.log(JSON.stringify({ control: Number(port) })));

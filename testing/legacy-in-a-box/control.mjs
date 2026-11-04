// The box's control for the persona drivers (run.sh personas starts it on the host): the runner's steps and the box's
// reads a driver needs at its own moments, each one run.sh subcommand. POST /<name>?<param>=<value> runs
// `run.sh <name> [<value>]` and answers its stdout (200) or its stderr (500). Names:
//   gate [person]            the runner's allow-handover (with a person: that person's own gate, `--person`)
//   owner <conversation>     legacy | owned
//   native-turns <conversation>  the conversation's person turns in magpie
//   sandbox <conversation>   the old jarvis.sandboxes row's status (none, running, gone)
//   native-files <tag>       the magpie conversation whose first message holds <tag>: id, owner and its files
//   old-model <tag>          the old mock model's requests whose prompt holds <tag> (JSON)
//   new-model <tag>          the new mock model's requests that hold <tag> (JSON)
import { execFile } from "node:child_process";
import http from "node:http";

const [script, port] = process.argv.slice(2);
const PARAMS = { gate: "person", owner: "conversation", "native-turns": "conversation", sandbox: "conversation",
  "native-files": "tag", "old-model": "tag", "new-model": "tag" };
const VALID = {
  conversation: /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/,
  person: /^[a-z0-9.+-]+@[a-z0-9.-]+$/,
  tag: /^[0-9a-z][0-9a-z.-]{3,63}$/,
};
http.createServer((req, res) => {
  const u = new URL(req.url, "http://x");
  const name = u.pathname.slice(1);
  const key = PARAMS[name];
  const v = key ? u.searchParams.get(key) : null;
  if (req.method !== "POST" || !key || (v && !VALID[key].test(v))) { res.writeHead(404); return res.end(); }
  execFile("bash", [script, name, ...(v ? [v] : [])], { timeout: 600000, maxBuffer: 1 << 24 }, (err, stdout, stderr) => {
    res.writeHead(err ? 500 : 200, { "content-type": "text/plain" });
    res.end(err ? `${stderr}`.slice(-2000) : stdout);
  });
}).listen(Number(port), "0.0.0.0", () => console.log(JSON.stringify({ control: Number(port) })));

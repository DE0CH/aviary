// A stand-in for pigeon, for walking the built app's screens with no server behind it: serves a web export and answers
// the Paseo protocol on /paseo-ws with pigeon's own recorded answers (pigeon-answers.json) filled with the sample
// content of content.mjs, plus the few REST routes the screens read.
//
//   import { startStandIn } from "./stand-in.mjs";  const { url, close } = await startStandIn({ appDir, port });
//
// With `appScheme` (a native app's scheme, app.config.js) it also answers the app's sign-in like pigeon's
// /api/jarvis/app-signin after Access: straight back to the app with a token, so the Android app's emulator test
// (scripts/android-app/emulator-test.mjs) signs in to it. `host` is where it listens (default 127.0.0.1); `quiet`
// answers with nothing running (content.mjs `quiet`).
import http from "node:http";
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";
import * as content from "./content.mjs";

const require = createRequire(path.join(path.dirname(new URL(import.meta.url).pathname), "../../package.json"));
const { WebSocketServer } = require("ws");

const here = path.dirname(new URL(import.meta.url).pathname);
const answers = JSON.parse(fs.readFileSync(path.join(here, "pigeon-answers.json"), "utf8"));
const clone = (value) => JSON.parse(JSON.stringify(value));
// the answer's type for a request's type; subscribe_terminals is answered by terminals_changed
const ANSWER_TYPE = { subscribe_terminals_request: "terminals_changed" };
function answerType(requestType) {
  if (ANSWER_TYPE[requestType]) return ANSWER_TYPE[requestType];
  if (requestType.endsWith("_request")) return requestType.replace(/_request$/, "_response");
  if (requestType.endsWith(".request")) return requestType.replace(/\.request$/, ".response");
  return null;
}

const MIME = {
  ".js": "text/javascript", ".html": "text/html", ".css": "text/css", ".json": "application/json", ".png": "image/png",
  ".svg": "image/svg+xml", ".ttf": "font/ttf", ".woff2": "font/woff2", ".wasm": "application/wasm", ".ico": "image/x-icon",
};

export function startStandIn({ appDir, port = 0, host = "127.0.0.1", appScheme = null, quiet = false }) {
  const root = path.resolve(appDir);
  if (quiet) content.quiet();
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, "http://stand-in");
    if (appScheme && url.pathname === "/api/jarvis/app-signin") {
      const state = encodeURIComponent(url.searchParams.get("state") ?? "");
      res.writeHead(302, { location: `${appScheme}://signed-in?state=${state}#token=stand-in` });
      return res.end();
    }
    if (url.pathname.startsWith("/api/")) {
      const out = content.rest(req.method, url.pathname);
      if (out === undefined) {
        res.writeHead(404, { "content-type": "application/json" });
        return res.end('{"error":"not here"}');
      }
      if (out.svg) {
        res.writeHead(200, { "content-type": "image/svg+xml" });
        return res.end(out.svg);
      }
      res.writeHead(200, { "content-type": "application/json" });
      return res.end(JSON.stringify(out));
    }
    let file = path.join(root, decodeURIComponent(url.pathname));
    if (!file.startsWith(root) || !fs.existsSync(file) || fs.statSync(file).isDirectory()) file = path.join(root, "index.html");
    res.writeHead(200, { "content-type": MIME[path.extname(file)] ?? "application/octet-stream", "cache-control": "no-store" });
    // a visitor without Access's sign-in gets the signed-out app (DE0CH/yyc-code access.tf): a header stands in for that
    if (file.endsWith("index.html") && req.headers["x-signed-out"]) {
      return res.end(fs.readFileSync(file, "utf8").replace("<head>", '<head><meta name="yyc-signed-out" content="1">'));
    }
    fs.createReadStream(file).pipe(res);
  });
  const sockets = new WebSocketServer({ server, path: "/paseo-ws" });
  sockets.on("connection", (ws) => {
    const send = (message) => ws.send(JSON.stringify({ type: "session", message }));
    ws.on("message", (raw) => {
      const frame = JSON.parse(raw.toString());
      if (frame.type === "hello") return send(content.status(clone(answers.status)));
      if (frame.type === "ping") return ws.send(JSON.stringify({ type: "pong" }));
      const message = frame.type === "session" ? frame.message : frame;
      const type = answerType(message.type);
      const template = type ? clone(answers.responses[type] ?? null) : null;
      if (content.answer(message, send, template)) return;
      if (template) {
        template.payload.requestId = message.requestId;
        return send(template);
      }
      if (message.requestId) {
        send({ type: "rpc_error", payload: { code: "unsupported", error: "Not available here", requestId: message.requestId, requestType: message.type } });
      }
    });
  });
  return new Promise((resolve) => {
    server.listen(port, host, () => {
      const { port: bound } = server.address();
      resolve({ url: `http://127.0.0.1:${bound}`, close: () => new Promise((done) => {
        // a client still connected (the app) would hold the server open: end it
        for (const client of sockets.clients) client.terminate();
        sockets.close();
        server.close(done);
        server.closeAllConnections();
      }) });
    });
  });
}

#!/usr/bin/env node
/**
 * End to end, on a built CLI: the host's thinking level reaches the model, "none" (thinking off) included.
 *
 *   node de0ch/thinking-level.e2e.mjs PATH_TO_QWEN
 *
 * Runs the CLI as aviary's supervisor does (stream-json both ways, yolo, `initialize` first, the model listed as
 * aviary's settings list it) against a stand-in model on 127.0.0.1 that keeps the `reasoning` of every request.
 * The host sets the level with the control request `set_effort` before a turn, and the check is that the turn's
 * request names it as `reasoning.effort`, which is where aviary's gateway reads it: a level, then "none", then a
 * level again and "none" again, in one process. A request that names no level gets the gateway's default for the
 * model, so "none" left out is thinking on (YYC AI, 2026-11-16).
 */
import { spawn } from 'node:child_process';
import console from 'node:console';
import { randomUUID } from 'node:crypto';
import { mkdirSync, mkdtempSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';
import { clearTimeout, setTimeout } from 'node:timers';

const qwen = process.argv[2];
if (!qwen) {
  console.error('usage: thinking-level.e2e.mjs PATH_TO_QWEN');
  process.exit(2);
}

const WAIT_MS = 60_000;
// A model of YYC AI's, as its requests name it.
const MODEL = 'deepseek/deepseek-v4.1-flash-1017';

/** Every request the stand-in model got: what it says of thinking, and whether it is the agent's (it has tools). */
const requests = [];
let calls = 0;

const model = createServer((req, res) => {
  let body = '';
  req.on('data', (chunk) => (body += chunk));
  req.on('end', () => {
    if (req.method !== 'POST' || !req.url.endsWith('/chat/completions')) {
      res.writeHead(404).end();
      return;
    }
    const request = JSON.parse(body);
    requests.push({
      agent: Array.isArray(request.tools) && request.tools.length > 0,
      reasoning: request.reasoning,
      reasoning_effort: request.reasoning_effort,
      thinking: request.thinking,
      enable_thinking: request.enable_thinking,
      chat_template_kwargs: request.chat_template_kwargs,
    });
    calls += 1;
    const base = { id: `chatcmpl-${calls}`, created: 1, model: request.model };
    const message = { role: 'assistant', content: 'ok' };
    const usage = { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 };
    if (!request.stream) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          ...base,
          object: 'chat.completion',
          choices: [{ index: 0, message, finish_reason: 'stop' }],
          usage,
        }),
      );
      return;
    }
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    const chunk = (choices, extra = {}) =>
      res.write(
        `data: ${JSON.stringify({ ...base, object: 'chat.completion.chunk', choices, ...extra })}\n\n`,
      );
    chunk([{ index: 0, delta: message, finish_reason: null }]);
    chunk([{ index: 0, delta: {}, finish_reason: 'stop' }]);
    chunk([], { usage });
    res.end('data: [DONE]\n\n');
  });
});
await new Promise((resolve) => model.listen(0, '127.0.0.1', resolve));
const port = model.address().port;

const home = mkdtempSync(join(tmpdir(), 'qwen-e2e-home-'));
const work = mkdtempSync(join(tmpdir(), 'qwen-e2e-work-'));
mkdirSync(join(home, '.qwen'));
writeFileSync(
  join(home, '.qwen', 'settings.json'),
  JSON.stringify({
    modelProviders: {
      openai: [
        {
          id: MODEL,
          name: 'e2e',
          envKey: 'QWEN_E2E_KEY',
          baseUrl: `http://127.0.0.1:${port}/v1`,
          generationConfig: {
            timeout: 600_000,
            modalities: { image: true, audio: true, video: true },
            contextWindowSize: 1_048_576,
          },
        },
      ],
    },
    security: { auth: { selectedType: 'openai' } },
    model: { name: MODEL },
    tools: { approvalMode: 'yolo' },
    telemetry: { enabled: false },
    general: { disableAutoUpdate: true, disableUpdateNag: true },
  }),
);

const env = {
  ...process.env,
  HOME: home,
  QWEN_E2E_KEY: 'e2e',
  QWEN_CODE_SUPPRESS_YOLO_WARNING: '1',
};
for (const name of Object.keys(env)) {
  if (/^(https?|all)_proxy$/i.test(name)) delete env[name];
}
env.NO_PROXY = env.no_proxy = 'localhost,127.0.0.1,::1';

const sessionId = randomUUID();
let current;

function fail(message) {
  console.error(`FAILED: ${message}`);
  console.error('--- requests to the model');
  for (const request of requests) console.error(JSON.stringify(request));
  if (current) {
    console.error('--- stdout lines');
    for (const line of current.lines) {
      if (line.type !== 'stream_event')
        console.error(JSON.stringify(line).slice(0, 400));
    }
    console.error('--- stderr');
    console.error(current.stderr().slice(-4000));
    current.child.kill('SIGKILL');
  }
  process.exit(1);
}

function check(condition, message) {
  if (!condition) fail(message);
}

/** The CLI process of the session. */
function start() {
  const child = spawn(
    qwen,
    [
      '--output-format',
      'stream-json',
      '--include-partial-messages',
      '--input-format',
      'stream-json',
      '--approval-mode',
      'yolo',
      '--model',
      MODEL,
      '--session-id',
      sessionId,
    ],
    { cwd: work, env, stdio: ['pipe', 'pipe', 'pipe'] },
  );
  const lines = [];
  const waiters = [];
  let stderr = '';
  let rest = '';
  child.stderr.on('data', (chunk) => (stderr += chunk));
  child.stdout.on('data', (chunk) => {
    rest += chunk;
    for (let at = rest.indexOf('\n'); at >= 0; at = rest.indexOf('\n')) {
      const raw = rest.slice(0, at);
      rest = rest.slice(at + 1);
      if (!raw.trim()) continue;
      try {
        lines.push(JSON.parse(raw));
      } catch {
        fail(`stdout line is not JSON: ${raw}`);
      }
      for (const waiter of [...waiters]) waiter();
    }
  });
  const exited = new Promise((resolve) =>
    child.on('exit', (code) => resolve(code)),
  );
  const send = (value) => child.stdin.write(`${JSON.stringify(value)}\n`);
  function lineAfter(from, what, test) {
    return new Promise((resolve) => {
      const look = () => {
        for (let i = from; i < lines.length; i++) {
          if (test(lines[i])) {
            clearTimeout(timer);
            waiters.splice(waiters.indexOf(look), 1);
            resolve(i);
            return true;
          }
        }
        return false;
      };
      const timer = setTimeout(
        () => fail(`no ${what} within ${WAIT_MS / 1000} s`),
        WAIT_MS,
      );
      waiters.push(look);
      look();
    });
  }
  const cli = {
    child,
    lines,
    stderr: () => stderr,
    async initialize() {
      const id = `e2e-${randomUUID()}`;
      send({
        type: 'control_request',
        request_id: id,
        request: { subtype: 'initialize' },
      });
      const at = await lineAfter(
        0,
        'answer to initialize',
        (line) =>
          line.type === 'control_response' && line.response?.request_id === id,
      );
      check(lines[at].response.subtype === 'success', 'initialize was refused');
    },
    /** The control request `set_effort`, and its answer. */
    async setEffort(effort) {
      const id = `e2e-${randomUUID()}`;
      const from = lines.length;
      send({
        type: 'control_request',
        request_id: id,
        request: { subtype: 'set_effort', effort },
      });
      const at = await lineAfter(
        from,
        `answer to set_effort ${effort}`,
        (line) =>
          line.type === 'control_response' && line.response?.request_id === id,
      );
      return lines[at].response;
    },
    /** Say `text` and wait for the turn's result line. */
    async turn(text) {
      const from = lines.length;
      send({
        type: 'user',
        message: { role: 'user', content: text },
        parent_tool_use_id: null,
      });
      const at = await lineAfter(
        from,
        `result of "${text}"`,
        (line) => line.type === 'result',
      );
      return lines[at];
    },
    async end() {
      child.stdin.end();
      const code = await Promise.race([
        exited,
        new Promise((resolve) =>
          setTimeout(() => resolve('still running'), 30_000),
        ),
      ]);
      check(code === 0, `the CLI ended with ${code}`);
    },
  };
  current = cli;
  return cli;
}

const cli = start();
await cli.initialize();

/** The `reasoning` of each request the agent made for the turn `text`. */
async function turn(text) {
  const before = requests.length;
  const result = await cli.turn(text);
  check(
    result.is_error === false && result.subtype === 'success',
    `"${text}" ended ${result.subtype}: ${JSON.stringify(result.error ?? result.result)}`,
  );
  const sent = requests.slice(before).filter((request) => request.agent);
  check(sent.length > 0, `"${text}" reached no model`);
  return sent.map((request) => request.reasoning);
}

// a level, thinking off, a level again, thinking off again: each turn's requests name what was set before it
for (const [effort, text] of [
  ['high', 'one'],
  ['none', 'two'],
  ['low', 'three'],
  ['none', 'four'],
]) {
  const answer = await cli.setEffort(effort);
  check(
    answer.subtype === 'success',
    `set_effort ${effort} was refused: ${JSON.stringify(answer.error)}`,
  );
  const sent = await turn(text);
  check(
    sent.every((reasoning) => reasoning?.effort === effort),
    `after set_effort ${effort} the requests' reasoning is ${JSON.stringify(sent)}`,
  );
}
// the next turn, with nothing set in between, keeps it
const kept = await turn('five');
check(
  kept.every((reasoning) => reasoning?.effort === 'none'),
  `the turn after "four" sent ${JSON.stringify(kept)}`,
);
await cli.end();

model.close();
console.log(
  `ok: ${requests.length} requests; "none" and the levels around it reached the model as reasoning.effort`,
);
process.exit(0);

#!/usr/bin/env node
/**
 * End to end, on a built CLI: a turn cut off by its host losing the machine is continued when the host asks.
 *
 *   node de0ch/continue-after-lost-host.e2e.mjs PATH_TO_QWEN
 *
 * When hermit releases a sandbox under a running turn, aviary's supervisor closes Qwen Code's stdin and sends
 * SIGTERM to its process group. An operator's continue then starts Qwen Code again on the session and sends
 * `continue_last_turn`. Checked against a stand-in model:
 *   1. the turn's shell call (a long command the shell tool allows; a plain `sleep` it refuses at once) is cut by
 *      what a release does;
 *   2. a new process resuming the session accepts `continue_last_turn`;
 *   3. its request carries the cut call's result as the last message, and the reply and a `result` follow.
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
  console.error('usage: continue-after-lost-host.e2e.mjs PATH_TO_QWEN');
  process.exit(2);
}

const WAIT_MS = 60_000;
const RUN = 'run the long job';
const AFTER = 'It went on after the cut.';

/** The text of a chat message, whatever shape its content has. */
function textOf(message) {
  if (typeof message?.content === 'string') return message.content;
  if (!Array.isArray(message?.content)) return '';
  return message.content.map((part) => part.text ?? '').join('');
}

/** Every request the stand-in model got: its text and how many tool results it carries. */
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
    const messages = request.messages ?? [];
    const last = messages[messages.length - 1];
    requests.push({
      text: messages.map(textOf).join('\n'),
      toolResults: messages.filter((m) => m.role === 'tool').length,
      lastRole: last?.role,
    });
    calls += 1;
    const base = { id: `chatcmpl-${calls}`, created: 1, model: request.model };
    const answer =
      last?.role === 'user' && textOf(last).includes(RUN)
        ? {
            tool: 'run_shell_command',
            args: {
              command: 'python3 -c "import time; time.sleep(120)"',
              description: 'the long job',
            },
          }
        : { text: last?.role === 'tool' ? AFTER : 'ok' };
    const delta = answer.tool
      ? {
          role: 'assistant',
          tool_calls: [
            {
              index: 0,
              id: `call_e2e_${calls}`,
              type: 'function',
              function: {
                name: answer.tool,
                arguments: JSON.stringify(answer.args),
              },
            },
          ],
        }
      : { role: 'assistant', content: answer.text };
    const finish = answer.tool ? 'tool_calls' : 'stop';
    const usage = { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 };
    if (!request.stream) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          ...base,
          object: 'chat.completion',
          choices: [{ index: 0, message: delta, finish_reason: finish }],
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
    chunk([{ index: 0, delta, finish_reason: null }]);
    chunk([{ index: 0, delta: {}, finish_reason: finish }]);
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
          id: 'e2e',
          name: 'e2e',
          envKey: 'QWEN_E2E_KEY',
          baseUrl: `http://127.0.0.1:${port}/v1`,
          generationConfig: {
            modalities: { image: true, audio: true, video: true },
          },
        },
      ],
    },
    security: { auth: { selectedType: 'openai' } },
    model: { name: 'e2e' },
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
  for (const request of requests) {
    console.error(
      JSON.stringify({ ...request, text: request.text.slice(-600) }),
    );
  }
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

/** One CLI process on the session: `how` is `--session-id` for the first, `--resume` after. */
function start(how) {
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
      'e2e',
      how,
      sessionId,
    ],
    { cwd: work, env, stdio: ['pipe', 'pipe', 'pipe'], detached: true },
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
    /** A control request's answer (`response.response`) and where the lines after it begin. */
    async control(subtype) {
      const id = `e2e-${randomUUID()}`;
      const from = lines.length;
      send({ type: 'control_request', request_id: id, request: { subtype } });
      const at = await lineAfter(
        from,
        `answer to ${subtype}`,
        (line) =>
          line.type === 'control_response' && line.response?.request_id === id,
      );
      check(
        lines[at].response.subtype === 'success',
        `${subtype} was refused: ${JSON.stringify(lines[at].response)}`,
      );
      return { answer: lines[at].response.response, from: at + 1 };
    },
    /** The first `result` line from `from` on. */
    async resultAfter(from, what) {
      const at = await lineAfter(from, what, (line) => line.type === 'result');
      return lines[at];
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

// 1. the turn's shell call is cut the way a released sandbox cuts it
let cli = start('--session-id');
await cli.initialize();
const from = cli.lines.length;
cli.child.stdin.write(
  `${JSON.stringify({ type: 'user', message: { role: 'user', content: RUN }, parent_tool_use_id: null })}\n`,
);
const startedAt = Date.now();
while (
  !cli.lines
    .slice(from)
    .some(
      (l) =>
        l.type === 'assistant' &&
        JSON.stringify(l).includes('run_shell_command'),
    )
) {
  check(Date.now() - startedAt < WAIT_MS, 'the shell call never started');
  await new Promise((r) => setTimeout(r, 100));
}
await new Promise((r) => setTimeout(r, 1500));
cli.child.stdin.end();
process.kill(-cli.child.pid, 'SIGTERM');
await Promise.race([
  new Promise((r) => cli.child.on('exit', r)),
  new Promise((r) => setTimeout(r, 15_000)),
]);

// 2. a new process on the session accepts the request to go on
cli = start('--resume');
await cli.initialize();
// 3. accepted; the cut call's result goes, a reply and a result follow
const before = requests.length;
const id = `e2e-${randomUUID()}`;
const at0 = cli.lines.length;
cli.child.stdin.write(
  `${JSON.stringify({ type: 'control_request', request_id: id, request: { subtype: 'continue_last_turn' } })}\n`,
);
const deadline = Date.now() + WAIT_MS;
let answer;
while (Date.now() < deadline) {
  const line = cli.lines
    .slice(at0)
    .find(
      (l) => l.type === 'control_response' && l.response?.request_id === id,
    );
  if (line) {
    answer = line.response.response;
    break;
  }
  await new Promise((r) => setTimeout(r, 100));
}
check(
  answer?.accepted === true,
  `continue_last_turn after the cut answered ${JSON.stringify(answer)}`,
);
const result = await cli.resultAfter(at0, 'result of the continuation');
check(
  result.is_error === false && result.result === AFTER,
  `the continuation ended ${JSON.stringify(result)}`,
);
const sent = requests.slice(before);
check(
  sent.length >= 1 && sent[0].toolResults >= 1,
  `the continuation sent ${JSON.stringify(sent[0] ?? null)}`,
);
check(
  sent[0].lastRole === 'tool',
  `its request ends with ${sent[0].lastRole}, not the cut call's result`,
);
await cli.end();
model.close();
console.log(`ok: ${calls} model calls`);
process.exit(0);

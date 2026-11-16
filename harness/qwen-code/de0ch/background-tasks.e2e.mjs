#!/usr/bin/env node
/**
 * End to end, on a built CLI: what a stream-json host (aviary's supervisor) sees of a background shell and of a
 * monitor.
 *
 *   node de0ch/background-tasks.e2e.mjs PATH_TO_QWEN        (QWEN_E2E_LINES=FILE keeps the stdout lines)
 *
 * Runs the CLI as the supervisor does (stream-json both ways, yolo, `initialize` first) against a stand-in model
 * on 127.0.0.1 that starts a background shell or a monitor when asked to, and checks:
 *   1. the shell is reported (a `system` line `task_started` whose `data` names `local_bash` and the tool call's
 *      id) inside the turn that started it;
 *   2. when the shell ends, the model's notification is written as a user line, then `task_notification`, and the
 *      model gets a turn of its own;
 *   3. `stop_task` ends a running shell: `task_notification` with `cancelled`, and no turn for the model;
 *   4. `stop_task` answers `stopped: false` for a shell that has ended, and an error for an id no task has;
 *   5. a monitor is reported (`monitor`) inside its turn, each of its events is a `task_notification` with
 *      `running` and its end one with `completed`, each with a turn for the model;
 *   6. `stop_task` ends a running monitor: `task_notification` with `cancelled`, and no turn for the model.
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
  console.error('usage: background-tasks.e2e.mjs PATH_TO_QWEN');
  process.exit(2);
}

const SHELL = 'start a shell: ';
const MONITOR = 'start a monitor: ';
const WAIT_MS = 60_000;

/** The text of a chat message, whatever shape its content has. */
function textOf(message) {
  if (typeof message?.content === 'string') return message.content;
  if (!Array.isArray(message?.content)) return '';
  return message.content.map((part) => part.text ?? '').join('');
}

/** What the stand-in model answers to a request: a tool call or a text. */
function answerTo(messages) {
  const last = messages[messages.length - 1];
  if (last?.role === 'tool') return { text: 'It is running.' };
  const text = textOf(last);
  if (text.includes('<task-notification>')) return { text: 'Noted.' };
  if (last?.role !== 'user') return { text: 'ok' };
  const commandAfter = (marker) => {
    const at = text.indexOf(marker);
    if (at < 0) return undefined;
    return text
      .slice(at + marker.length)
      .split('\n')[0]
      .trim();
  };
  const shell = commandAfter(SHELL);
  if (shell) {
    return {
      tool: 'run_shell_command',
      args: { command: shell, is_background: true, description: 'e2e shell' },
    };
  }
  const monitor = commandAfter(MONITOR);
  if (monitor) {
    return {
      tool: 'monitor',
      args: { command: monitor, description: 'e2e monitor' },
    };
  }
  return { text: 'ok' };
}

const notified = [];
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
    const lastText = textOf(messages[messages.length - 1]);
    if (lastText.includes('<task-notification>')) notified.push(lastText);
    const answer = answerTo(messages);
    calls += 1;
    const id = `chatcmpl-${calls}`;
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
    const base = { id, created: 1, model: request.model };
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
    '--session-id',
    randomUUID(),
  ],
  { cwd: work, env, stdio: ['pipe', 'pipe', 'pipe'] },
);

/** Every stdout line so far, parsed, and the readers waiting for one. */
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
    let line;
    try {
      line = JSON.parse(raw);
    } catch {
      fail(`stdout line is not JSON: ${raw}`);
    }
    lines.push(line);
    for (const waiter of [...waiters]) waiter();
  }
});
const exited = new Promise((resolve) =>
  child.on('exit', (code) => resolve(code)),
);

function brief(line) {
  if (line.type === 'stream_event') return null;
  return JSON.stringify(line).slice(0, 400);
}

function fail(message) {
  console.error(`FAILED: ${message}`);
  console.error('--- stdout lines');
  for (const line of lines) {
    const text = brief(line);
    if (text) console.error(text);
  }
  console.error('--- stderr');
  console.error(stderr.slice(-4000));
  child.kill('SIGKILL');
  process.exit(1);
}

function check(condition, message) {
  if (!condition) fail(message);
}

/** The index of the first line from `from` on that `test` accepts, waiting for it. */
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

const send = (value) => child.stdin.write(`${JSON.stringify(value)}\n`);
const say = (text) =>
  send({
    type: 'user',
    message: { role: 'user', content: text },
    parent_tool_use_id: null,
  });
const isSystem = (subtype) => (line) =>
  line.type === 'system' && line.subtype === subtype;
const isResult = (line) => line.type === 'result';
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** A control request's answer: `{ ok, response | error }`. */
async function control(request) {
  const id = `e2e-${randomUUID()}`;
  const from = lines.length;
  send({ type: 'control_request', request_id: id, request });
  const at = await lineAfter(
    from,
    `answer to ${request.subtype}`,
    (line) =>
      line.type === 'control_response' && line.response?.request_id === id,
  );
  const { subtype, response, error } = lines[at].response;
  return { ok: subtype === 'success', response, error };
}

/** The id of the call of `tool` among the assistant lines in [from, to). */
function callId(tool, from, to) {
  for (const line of lines.slice(from, to)) {
    if (line.type !== 'assistant') continue;
    for (const part of line.message?.content ?? []) {
      if (part.type === 'tool_use' && part.name === tool) return part.id;
    }
  }
  return undefined;
}

/** Whether a turn ran among the lines from `from` on. */
const turnSince = (from) =>
  lines
    .slice(from)
    .some(
      (line) =>
        isResult(line) || line.type === 'assistant' || line.type === 'user',
    );

check((await control({ subtype: 'initialize' })).ok, 'initialize was refused');

// 1. a shell that ends by itself: reported inside its turn
let from = lines.length;
say(`${SHELL}sleep 2; echo e2e-finished`);
const firstResult = await lineAfter(from, 'result of the first turn', isResult);
const startedAt = await lineAfter(
  from,
  'task_started',
  isSystem('task_started'),
);
check(
  startedAt < firstResult,
  'task_started came after the result of the turn that started the shell',
);
const started = lines[startedAt].data ?? {};
check(started.task_type === 'local_bash', `task_type is ${started.task_type}`);
check(
  typeof started.task_id === 'string' && started.task_id,
  'task_started names no task_id',
);
check(
  started.tool_use_id &&
    started.tool_use_id === callId('run_shell_command', from, firstResult),
  `task_started names tool call ${started.tool_use_id}, not the shell's`,
);

// 2. its end: the model's notification as a user line, task_notification, a turn of the model's own
const noticeAt = await lineAfter(
  firstResult,
  'user line with the task notification',
  (line) =>
    line.type === 'user' &&
    JSON.stringify(line).includes('<task-notification>'),
);
const endedAt = await lineAfter(
  firstResult,
  'task_notification',
  isSystem('task_notification'),
);
const secondResult = await lineAfter(
  firstResult + 1,
  'result of the notification turn',
  isResult,
);
check(
  noticeAt < endedAt && endedAt < secondResult,
  'order is not: user line, task_notification, result',
);
const ended = lines[endedAt].data ?? {};
check(
  ended.task_id === started.task_id,
  'task_notification names another task',
);
check(ended.status === 'completed', `the shell ended as ${ended.status}`);
check(
  ended.tool_use_id === started.tool_use_id,
  'task_notification names another tool call',
);
check(
  typeof ended.output_file === 'string' && ended.output_file,
  'task_notification names no output_file',
);
check(
  notified.length === 1 && notified[0].includes(started.task_id),
  `the model was told of the end ${notified.length} times`,
);

// 3. a shell the host stops: no turn for the model
from = lines.length;
say(`${SHELL}sleep 600`);
const thirdResult = await lineAfter(from, 'result of the third turn', isResult);
const longAt = await lineAfter(
  from,
  'task_started of the long shell',
  isSystem('task_started'),
);
check(longAt < thirdResult, 'the long shell was reported after its turn');
const long = lines[longAt].data ?? {};
from = lines.length;
const stop = await control({ subtype: 'stop_task', task_id: long.task_id });
check(
  stop.ok && stop.response?.stopped === true,
  `stop_task answered ${JSON.stringify(stop)}`,
);
const stoppedAt = await lineAfter(
  from,
  'task_notification of the stopped shell',
  isSystem('task_notification'),
);
const stopped = lines[stoppedAt].data ?? {};
check(stopped.task_id === long.task_id, 'the stop ended another task');
check(
  stopped.status === 'cancelled',
  `the stopped shell ended as ${stopped.status}`,
);
await pause(3000);
check(notified.length === 1, 'the model was told of a shell the host stopped');
check(!turnSince(from), 'a turn ran after the host stopped the shell');

// 4. stop_task for a shell that has ended, and for an id no task has
const again = await control({ subtype: 'stop_task', task_id: long.task_id });
check(
  again.ok && again.response?.stopped === false,
  `the second stop_task answered ${JSON.stringify(again)}`,
);
const unknown = await control({
  subtype: 'stop_task',
  task_id: 'no-such-task',
});
check(
  !unknown.ok,
  `stop_task for an unknown id answered ${JSON.stringify(unknown)}`,
);

// 5. a monitor: reported inside its turn, its event and its end each a notification and a turn
from = lines.length;
let told = notified.length;
say(`${MONITOR}echo e2e-event; sleep 3`);
const monitorResult = await lineAfter(
  from,
  'result of the monitor turn',
  isResult,
);
const watchAt = await lineAfter(
  from,
  'task_started of the monitor',
  isSystem('task_started'),
);
check(watchAt < monitorResult, 'the monitor was reported after its turn');
const watch = lines[watchAt].data ?? {};
check(watch.task_type === 'monitor', `task_type is ${watch.task_type}`);
check(
  watch.tool_use_id &&
    watch.tool_use_id === callId('monitor', from, monitorResult),
  `task_started names tool call ${watch.tool_use_id}, not the monitor's`,
);
const ofWatch = (test) => (line) =>
  isSystem('task_notification')(line) &&
  line.data?.task_id === watch.task_id &&
  test(line.data.status);
const eventAt = await lineAfter(
  from,
  'task_notification of the monitor event',
  ofWatch((status) => status === 'running'),
);
const eventResult = await lineAfter(
  eventAt,
  'result of the event turn',
  isResult,
);
const closedAt = await lineAfter(
  from,
  'task_notification of the monitor end',
  ofWatch((status) => status !== 'running'),
);
check(eventAt < closedAt, 'the monitor ended before its event was reported');
check(
  lines[closedAt].data.status === 'completed',
  `the monitor ended as ${lines[closedAt].data.status}`,
);
await lineAfter(
  Math.max(closedAt, eventResult + 1),
  'result of the turn for the monitor end',
  isResult,
);
check(
  notified.length === told + 2 &&
    notified[told].includes('e2e-event') &&
    notified[told + 1].includes(watch.task_id),
  `the model was told of the monitor ${notified.length - told} times`,
);

// 6. a monitor the host stops: no turn for the model
from = lines.length;
told = notified.length;
say(`${MONITOR}sleep 600`);
const idleResult = await lineAfter(
  from,
  'result of the second monitor turn',
  isResult,
);
const idleAt = await lineAfter(
  from,
  'task_started of the second monitor',
  isSystem('task_started'),
);
check(idleAt < idleResult, 'the second monitor was reported after its turn');
const idle = lines[idleAt].data ?? {};
from = lines.length;
const halt = await control({ subtype: 'stop_task', task_id: idle.task_id });
check(
  halt.ok && halt.response?.stopped === true,
  `stop_task of the monitor answered ${JSON.stringify(halt)}`,
);
const haltedAt = await lineAfter(
  from,
  'task_notification of the stopped monitor',
  isSystem('task_notification'),
);
const halted = lines[haltedAt].data ?? {};
check(halted.task_id === idle.task_id, 'the stop ended another monitor');
check(
  halted.status === 'cancelled',
  `the stopped monitor ended as ${halted.status}`,
);
await pause(3000);
check(
  notified.length === told,
  'the model was told of a monitor the host stopped',
);
check(!turnSince(from), 'a turn ran after the host stopped the monitor');
check(
  lines.slice(from).filter(isSystem('task_notification')).length === 1,
  'the stopped monitor ended more than once',
);

child.stdin.end();
const code = await Promise.race([
  exited,
  pause(30_000).then(() => 'still running'),
]);
check(code === 0, `the CLI ended with ${code}`);
model.close();
// the lines themselves, for whoever writes a host against them
if (process.env.QWEN_E2E_LINES) {
  writeFileSync(
    process.env.QWEN_E2E_LINES,
    lines.map((line) => JSON.stringify(line)).join('\n') + '\n',
  );
}
console.log(`ok: ${lines.length} stdout lines, ${calls} model calls`);
process.exit(0);

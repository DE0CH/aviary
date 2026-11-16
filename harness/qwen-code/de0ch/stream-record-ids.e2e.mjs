#!/usr/bin/env node
/**
 * End to end, on a built CLI: the stream names a reply as the transcript records it.
 *
 *   node de0ch/stream-record-ids.e2e.mjs PATH_TO_QWEN
 *
 * aviary shows a reply as it is written from the `stream_event` lines and then finds the reply in the transcript;
 * it matches the two by (message id, block index), as Claude Code's stream and transcript allow. Upstream names the
 * stream of a reply by an id of its own and numbers every block 0, while the transcript's record has its own uuid
 * and its parts in order: the finished reply never met its streamed text, and in YYC AI every answer stood twice in
 * the chat for a moment, the chat jumping down and back (2026-11-16).
 *
 * Runs the CLI as aviary's supervisor does against a stand-in model on 127.0.0.1 whose first reply thinks, says
 * something and calls a tool, and whose second reply is text. The check: every block the stream showed for the
 * main agent (thinking, text, tool call) is the part at that index of the transcript record with that uuid, with
 * the same text; and both replies were streamed.
 */
import console from 'node:console';
import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import process from 'node:process';
import { clearTimeout, setTimeout } from 'node:timers';

const qwen = process.argv[2];
if (!qwen) {
  console.error('usage: stream-record-ids.e2e.mjs PATH_TO_QWEN');
  process.exit(2);
}

const WAIT_MS = 60_000;
const THOUGHT = ['The person wants a command run. ', 'I will run it.'];
const SAID = ['I will run ', 'the command now.'];
const ANSWER = ['The command ', 'printed nothing.'];

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
    requests.push({ last: last?.role, stream: Boolean(request.stream) });
    calls += 1;
    const base = { id: `chatcmpl-${calls}`, created: 1, model: request.model };
    const usage = { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 };
    const afterTool = last?.role === 'tool';
    if (!request.stream) {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(
        JSON.stringify({
          ...base,
          object: 'chat.completion',
          choices: [
            {
              index: 0,
              message: { role: 'assistant', content: 'ok' },
              finish_reason: 'stop',
            },
          ],
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
    const delta = (d) => chunk([{ index: 0, delta: d, finish_reason: null }]);
    if (afterTool) {
      for (const text of ANSWER) delta({ role: 'assistant', content: text });
      chunk([{ index: 0, delta: {}, finish_reason: 'stop' }]);
    } else {
      for (const text of THOUGHT)
        delta({ role: 'assistant', reasoning_content: text });
      for (const text of SAID) delta({ role: 'assistant', content: text });
      delta({
        role: 'assistant',
        tool_calls: [
          {
            index: 0,
            id: `call_e2e_${calls}`,
            type: 'function',
            function: {
              name: 'run_shell_command',
              arguments: JSON.stringify({ command: 'true' }),
            },
          },
        ],
      });
      chunk([{ index: 0, delta: {}, finish_reason: 'tool_calls' }]);
    }
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
    console.error(JSON.stringify(request));
  }
  if (current) {
    console.error('--- stdout lines');
    for (const line of current.lines) {
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

const cli = start('--session-id');
await cli.initialize();
const result = await cli.turn('run the command');
check(
  result.is_error === false && result.subtype === 'success',
  `the turn ended ${result.subtype}: ${JSON.stringify(result.error ?? result.result)}`,
);
await cli.end();

// what the stream showed of the main agent's replies: message id -> block index -> {type, text}
const streamed = new Map();
let message = null;
for (const line of cli.lines) {
  if (line.type !== 'stream_event' || line.parent_tool_use_id !== null)
    continue;
  const event = line.event;
  if (event.type === 'message_start') {
    message = event.message.id;
    check(
      !streamed.has(message),
      `two replies were streamed under the id ${message}`,
    );
    streamed.set(message, new Map());
  } else if (event.type === 'content_block_start') {
    check(message, 'a block started before any message_start');
    const blocks = streamed.get(message);
    check(
      !blocks.has(event.index),
      `block ${event.index} of ${message} started twice: the blocks of a reply are not numbered through`,
    );
    blocks.set(event.index, { type: event.content_block.type, text: '' });
  } else if (event.type === 'content_block_delta') {
    const block = streamed.get(message)?.get(event.index);
    check(block, `a delta for block ${event.index}, which never started`);
    block.text += event.delta.text ?? event.delta.thinking ?? '';
  }
}
check(
  streamed.size === 2,
  `${streamed.size} replies were streamed, not 2 (${requests.length} requests reached the model)`,
);

function files(dir) {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    return statSync(path).isDirectory() ? files(path) : [path];
  });
}
const transcript = files(join(home, '.qwen')).find((path) =>
  path.endsWith(`${sessionId}.jsonl`),
);
check(transcript, 'the session left no transcript');
const records = new Map(
  readFileSync(transcript, 'utf8')
    .split('\n')
    .filter((line) => line.trim())
    .map((line) => JSON.parse(line))
    .filter((record) => record.type === 'assistant')
    .map((record) => [record.uuid, record.message?.parts ?? []]),
);
const kind = (part) =>
  part.functionCall ? 'tool_use' : part.thought ? 'thinking' : 'text';
let blocks = 0;
for (const [id, shown] of streamed) {
  const parts = records.get(id);
  check(
    parts,
    `the stream named a reply ${id}; the transcript's assistant records are ${[...records.keys()].join(', ')}`,
  );
  for (const [index, block] of shown) {
    const part = parts[index];
    check(part, `the stream showed block ${index} of ${id}; its record has ${parts.length} parts`);
    check(
      kind(part) === block.type,
      `block ${index} of ${id} was streamed as ${block.type} and recorded as ${kind(part)}`,
    );
    if (block.type !== 'tool_use') {
      check(
        part.text === block.text,
        `block ${index} of ${id} was streamed as ${JSON.stringify(block.text)} and recorded as ${JSON.stringify(part.text)}`,
      );
    }
    blocks += 1;
  }
}
const first = [...streamed.values()][0];
check(
  [...first.values()].map((b) => b.type).join(',') === 'thinking,text,tool_use',
  `the first reply was streamed as ${[...first.values()].map((b) => b.type).join(',')}`,
);
check(
  first.get(0).text === THOUGHT.join('') && first.get(1).text === SAID.join(''),
  'the first reply was not streamed whole',
);

model.close();
console.log(
  `ok: ${streamed.size} replies, ${blocks} streamed blocks, each the part of its transcript record`,
);
process.exit(0);

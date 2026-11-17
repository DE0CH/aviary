#!/usr/bin/env node
/**
 * End to end, on a built CLI: a host can have the agent go on from a turn that was cut off, with no message.
 *
 *   node de0ch/continue-last-turn.e2e.mjs PATH_TO_QWEN
 *
 * aviary's supervisor sends the control request `continue_last_turn` when an operator continues a conversation
 * whose turn failed (YYC AI, 2026-11-16: a turn failed on the request that carried its tool results, and the two
 * "continue" messages sent after it failed as well). This runs the CLI as the supervisor does against a stand-in
 * model and checks what the supervisor relies on:
 *   1. a turn fails on the request that carries its tool results (the stand-in answers 401), and so do two
 *      messages after it;
 *   2. a new process resuming the session takes `continue_last_turn`: `accepted`, `interrupted_prompt`;
 *   3. it sends the tool results again with no message of the person's: neither of the two later messages is in
 *      the request, and the video the provider refuses is replaced by the harness's notice (the rollback);
 *   4. the model's reply and a `result` follow as after a message, and the turn ends well;
 *   5. a second `continue_last_turn` is not accepted: nothing is cut off any more;
 *   6. the next message's request holds neither of the two failed messages.
 */
import { Buffer } from 'node:buffer';
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
  console.error('usage: continue-last-turn.e2e.mjs PATH_TO_QWEN');
  process.exit(2);
}

const WAIT_MS = 60_000;
const REASON = 'Upstream error from DeepInfra: Could not open video stream';
const OPEN = 'open the clip: ';

// An .mp4 as ffmpeg leaves it while it is still writing: the header and some of the picture, no index (moov).
const halfWritten = Buffer.concat([
  Buffer.from(
    '000000206674797069736f6d0000020069736f6d69736f32617663316d703431',
    'hex',
  ),
  Buffer.from('00000008667265650000ffff6d646174', 'hex'),
  Buffer.alloc(4096, 0x5a),
]);
const halfWrittenBase64 = halfWritten.toString('base64');

/** The text of a chat message, whatever shape its content has. */
function textOf(message) {
  if (typeof message?.content === 'string') return message.content;
  if (!Array.isArray(message?.content)) return '';
  return message.content.map((part) => part.text ?? '').join('');
}

/** Every request the stand-in model got: whether it carried the video, and all its text. */
const requests = [];
/** `broken`: every request that carries a tool result is answered 401, as a failure that keeps its data. */
let phase = 'broken';
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
    const carriesVideo = body.includes(halfWrittenBase64.slice(0, 200));
    const toolMessages = messages.filter((m) => m.role === 'tool');
    requests.push({
      carriesVideo,
      text: messages.map(textOf).join('\n'),
      toolResults: toolMessages.length,
      toolCalls: messages
        .filter((m) => m.role === 'assistant')
        .reduce((n, m) => n + (m.tool_calls?.length ?? 0), 0),
      stream: Boolean(request.stream),
    });
    calls += 1;
    const base = { id: `chatcmpl-${calls}`, created: 1, model: request.model };
    if (phase === 'broken' && toolMessages.length > 0) {
      res.writeHead(401, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ error: { message: 'no such key', code: 401 } }));
      return;
    }
    if (carriesVideo) {
      if (!request.stream) {
        res.writeHead(400, { 'content-type': 'application/json' });
        res.end(JSON.stringify({ error: { message: REASON, code: 400 } }));
        return;
      }
      res.writeHead(200, { 'content-type': 'text/event-stream' });
      res.write(
        `data: ${JSON.stringify({
          ...base,
          object: 'chat.completion.chunk',
          provider: 'DeepInfra',
          error: { message: REASON, code: 400 },
          choices: [
            { index: 0, delta: { content: '' }, finish_reason: 'error' },
          ],
        })}\n\n`,
      );
      res.end();
      return;
    }
    const lastText = textOf(last);
    const at = last?.role === 'user' ? lastText.indexOf(OPEN) : -1;
    const answer =
      at >= 0
        ? {
            tool: 'read_file',
            args: {
              file_path: lastText
                .slice(at + OPEN.length)
                .split('\n')[0]
                .trim(),
            },
          }
        : last?.role === 'tool'
          ? { text: 'I could not see the clip.' }
          : { text: 'ok' };
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
const clip = join(work, 'clip_0-92s.mp4');
writeFileSync(clip, halfWritten);

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

const LATER = 'please go on now';

// 1. the turn fails on the request that carries its tool result, and so do two messages after it
let cli = start('--session-id');
await cli.initialize();
const first = await cli.turn(`${OPEN}${clip}`);
check(first.is_error === true, 'the first turn did not fail');
for (const n of [1, 2]) {
  const later = await cli.turn(`${LATER} ${n}`);
  check(later.is_error === true, `message ${n} after the failure did not fail`);
}
await cli.end();

// 2. a new process on the session is asked to go on
phase = 'healthy';
cli = start('--resume');
await cli.initialize();
let before = requests.length;
const asked = await cli.control('continue_last_turn');
check(
  asked.answer?.accepted === true &&
    asked.answer?.interruption === 'interrupted_prompt',
  `continue_last_turn answered ${JSON.stringify(asked.answer)}`,
);

// 3, 4. the tool results again, no message; the refused video replaced; a reply and a result
const result = await cli.resultAfter(asked.from, 'result of the continuation');
check(
  result.is_error === false && result.subtype === 'success',
  `the continuation ended ${result.subtype}: ${JSON.stringify(result.error ?? result.result)}`,
);
check(
  result.result === 'I could not see the clip.',
  `the continuation's reply was ${JSON.stringify(result.result)}`,
);
const sent = requests.slice(before);
check(sent.length >= 2, `the continuation made ${sent.length} model call(s)`);
check(
  sent[0].carriesVideo && !sent.at(-1).carriesVideo,
  'the refused video was not replaced on the way',
);
for (const request of sent) {
  check(
    !request.text.includes(LATER),
    'a message sent after the failure was sent with the continuation',
  );
  check(
    request.toolCalls === 1 && request.toolResults === 1,
    `the continuation pairs ${request.toolCalls} tool call(s) with ${request.toolResults} result(s)`,
  );
}
check(
  sent.at(-1).text.includes(REASON),
  "the continuation's request does not carry the harness's notice of the refusal",
);
check(
  !cli.lines
    .slice(asked.from)
    .some(
      (line) => line.type === 'user' && JSON.stringify(line).includes(LATER),
    ),
  'the continuation wrote a message of the person',
);

// 5. nothing is cut off any more
const again = await cli.control('continue_last_turn');
check(
  again.answer?.accepted === false,
  `a second continue_last_turn answered ${JSON.stringify(again.answer)}`,
);

// 6. the conversation goes on without the failed messages
before = requests.length;
const next = await cli.turn('and now?');
check(next.is_error === false, `the next message ended ${next.subtype}`);
check(
  requests.slice(before).every((request) => !request.text.includes(LATER)),
  'a message that failed before the continuation came back with the next one',
);
await cli.end();

model.close();
console.log(`ok: ${calls} model calls`);
process.exit(0);

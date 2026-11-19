#!/usr/bin/env node
/**
 * End to end, on a built CLI: a video the model provider cannot open does not end the conversation.
 *
 *   node de0ch/upstream-refusal.e2e.mjs PATH_TO_QWEN
 *
 * Runs the CLI as aviary's supervisor does (stream-json both ways, yolo, `initialize` first) against a stand-in
 * model on 127.0.0.1 that refuses any request carrying one particular video the way OpenRouter passes on its
 * provider's refusal: HTTP 200, then an `error` frame with code 400 inside the stream. The agent opens that video
 * (a half-written .mp4) with read_file, as it did in YYC AI on 2026-11-16, and the check is:
 *   1. the turn ends well: the refused request is sent again with the video replaced by the harness's notice of
 *      the refusal (the provider's words in it), the tool call keeps its result, and the model answers that;
 *   2. the next message ("continue") is answered: no later request carries the video;
 *   3. a new process resuming the session sends no video either: the transcript followed the replacement;
 *   4. the transcript still holds the refused video, on a branch that is not read (for an audit).
 */
import { Buffer } from 'node:buffer';
import { spawn } from 'node:child_process';
import console from 'node:console';
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
  console.error('usage: upstream-refusal.e2e.mjs PATH_TO_QWEN');
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

// 1. the agent opens the half-written clip: the refusal becomes the tool's result and the turn ends well
let cli = start('--session-id');
await cli.initialize();
const first = await cli.turn(`${OPEN}${clip}`);
check(
  first.is_error === false && first.subtype === 'success',
  `the turn that opened the clip ended ${first.subtype}: ${JSON.stringify(first.error ?? first.result)}`,
);
const refusedAt = requests.findIndex((request) => request.carriesVideo);
check(
  refusedAt >= 0,
  'no request carried the video: read_file did not put it in the conversation',
);
check(
  requests.filter((request) => request.carriesVideo).length === 1,
  'the refused video was sent more than once',
);
const retried = requests[refusedAt + 1];
check(retried, 'nothing was sent after the refused request');
check(
  retried.text.includes(REASON),
  "the request after the refusal does not carry the provider's words",
);
check(
  retried.toolCalls === retried.toolResults && retried.toolResults === 1,
  `the request after the refusal pairs ${retried.toolCalls} tool call(s) with ${retried.toolResults} result(s)`,
);
check(
  first.result === 'I could not see the clip.',
  `the model's answer was ${JSON.stringify(first.result)}`,
);

// 2. the conversation goes on
let before = requests.length;
const second = await cli.turn('continue');
check(
  second.is_error === false,
  `"continue" ended ${second.subtype}: ${JSON.stringify(second.error)}`,
);
check(requests.length > before, '"continue" reached no model');
check(
  requests.slice(before).every((request) => !request.carriesVideo),
  '"continue" sent the refused video again',
);
check(
  requests.at(-1).text.includes(REASON),
  'the notice of the refusal left the conversation',
);
await cli.end();

// 3. a new process on the same session reads the replacement, not the video
cli = start('--resume');
await cli.initialize();
before = requests.length;
const third = await cli.turn('and now?');
check(
  third.is_error === false,
  `the resumed session ended ${third.subtype}: ${JSON.stringify(third.error)}`,
);
check(requests.length > before, 'the resumed session reached no model');
check(
  requests.slice(before).every((request) => !request.carriesVideo),
  'the resumed session sent the refused video: the transcript did not follow the replacement',
);
check(
  requests.at(-1).text.includes(REASON),
  'the resumed session lost the notice of the refusal',
);
await cli.end();

// 4. the transcript keeps what was refused
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
check(
  readFileSync(transcript, 'utf8').includes(halfWrittenBase64.slice(0, 200)),
  'the transcript no longer holds the refused video',
);

model.close();
console.log(
  `ok: ${calls} model calls, 1 refused; the transcript is ${transcript}`,
);
process.exit(0);

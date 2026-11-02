// Records a frame log of the CloudCLI Paseo bridge (DE0CH/claudecodeui server/modules/paseo), the server the shipped
// Paseo apps talk to today. It runs the bridge's own code in process, as its tests do: a socket-shaped object takes
// the place of the WebSocket, CloudCLI's session store is replaced by an in-memory one, and the timelines are built by
// the bridge's own SessionTimeline from CloudCLI messages. Every frame in both directions is written as NDJSON
// (`{"case", "dir", "frame"}`), the format validate-frames.mjs reads.
//
// Run by record.sh inside a claudecodeui checkout (it is copied next to the bridge's tests so the `@/` imports
// resolve):  npx tsx --tsconfig server/tsconfig.json server/modules/paseo/tests/record-cloudcli-bridge.ts OUT.ndjson
import { EventEmitter } from 'node:events';
import { writeFileSync } from 'node:fs';

import { sessionsService } from '@/modules/providers/index.js';

import { paseoHub } from '../paseo-hub.js';
import { handlePaseoConnection } from '../paseo-connection.js';
import { SessionTimeline } from '../paseo-timeline.js';

type AnyRecord = Record<string, any>;

const out = process.argv[2];
if (!out) throw new Error('usage: record-cloudcli-bridge.ts OUT.ndjson');

const lines: string[] = [];
let currentCase = 'setup';

class RecordingSocket extends EventEmitter {
  readonly OPEN = 1;
  readyState = 1;
  constructor(private readonly name: string) {
    super();
  }
  send(data: string) {
    lines.push(JSON.stringify({ case: `${this.name}: ${currentCase}`, dir: 'out', frame: JSON.parse(data) }));
  }
  /** A frame from the client; `outsideSchema` marks one no shipped client sends (the server's dispatch rules). */
  client(frame: unknown, outsideSchema = false) {
    lines.push(JSON.stringify({ case: `${this.name}: ${currentCase}`, dir: 'in', ...(outsideSchema ? { expect: 'invalid' } : {}), frame }));
    this.emit('message', Buffer.from(JSON.stringify(frame)));
  }
  raw(text: string) {
    this.emit('message', Buffer.from(text));
  }
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 25));

// ------------------------------------------------------------------------------------------------ the session store

type Row = { session_id: string; provider_session_id: string | null; custom_name: string | null; created_at: string; updated_at: string; deleted_at?: string | null; isArchived?: number };

const A = '0a5e5b9e-1c2d-4e3f-8a9b-000000000001'; // a conversation with a long history
const B = '0a5e5b9e-1c2d-4e3f-8a9b-000000000002'; // an untitled conversation
const C = '0a5e5b9e-1c2d-4e3f-8a9b-000000000003'; // archived (Recently deleted)
const NEW = '0a5e5b9e-1c2d-4e3f-8a9b-000000000004'; // created by the client
const MISSING = '0a5e5b9e-1c2d-4e3f-8a9b-0000000000ff';

const rows = new Map<string, Row>([
  [A, { session_id: A, provider_session_id: 'cc-a', custom_name: 'Quarterly figures', created_at: '2026-10-28 01:00:00', updated_at: '2026-10-29 09:30:00', isArchived: 0 }],
  [B, { session_id: B, provider_session_id: null, custom_name: null, created_at: '2026-10-29 02:00:00', updated_at: '2026-10-29 02:00:00', isArchived: 0 }],
  [C, { session_id: C, provider_session_id: 'cc-c', custom_name: '舊對話', created_at: '2026-10-20 03:00:00', updated_at: '2026-10-21 04:00:00', deleted_at: '2026-10-22 05:00:00', isArchived: 1 }],
]);
const running = new Set<string>();
let webOk = true;

const hub = paseoHub as any;
const sessions = sessionsService as any;
process.env.JARVIS_PRODUCT_NAME = 'Example AI';
process.env.JARVIS_MODEL = 'claude-opus-4-6';
process.env.JARVIS_MODEL_LABEL = 'Claude Opus 4.6';
hub.ensureStarted = () => {};
hub.listSessions = async () => [...rows.values()].filter((row) => row.isArchived !== 1);
hub.listDeleted = async () => [...rows.values()].filter((row) => row.isArchived === 1);
hub.getSession = async (id: string, options: { includeDeleted?: boolean } = {}) => {
  const row = rows.get(id);
  if (!row || (row.isArchived === 1 && !options.includeDeleted)) return null;
  return row;
};
hub.isRunning = (id: string) => running.has(id);
hub.activeTurn = (id: string) => (running.has(id) ? { turnId: 'turn-live', startedAt: '2026-10-29T09:30:00.000Z' } : null);
sessions.renameSessionById = async (id: string, title: string) => {
  const row = rows.get(id);
  if (!row) throw new Error('Session not found');
  row.custom_name = title;
};
sessions.deleteOrArchiveSessionById = async (id: string, options: AnyRecord = {}) => {
  const row = rows.get(id);
  if (!row) throw new Error('Session not found');
  if (options.force) rows.delete(id);
  else Object.assign(row, { isArchived: 1, deleted_at: '2026-10-29 10:00:00' });
};
sessions.restoreSessionById = async (id: string) => {
  const row = rows.get(id);
  if (row) Object.assign(row, { isArchived: 0, deleted_at: null });
};
sessions.createAppSession = async () => {
  rows.set(NEW, { session_id: NEW, provider_session_id: null, custom_name: null, created_at: '2026-10-29 11:00:00', updated_at: '2026-10-29 11:00:00', isArchived: 0 });
  return { sessionId: NEW };
};
// the hub's own send runs (the user row and turn_started go out), and its chat socket is absent so nothing runs
process.env.JARVIS_WEB_URL = 'http://web.test';
process.env.JARVIS_CC_TOKEN = 'test-token';
process.env.JARVIS_CC_OWNER = 'person@example.com';
globalThis.fetch = (async () => (webOk ? new Response('{"ok":true}', { status: 200 }) : new Response('down', { status: 503 }))) as typeof fetch;

// ------------------------------------------------------------------------------------------------------ timelines

const toRelative = (value: unknown): string | null =>
  typeof value === 'string' && value.startsWith('/workspace/') ? value.slice('/workspace/'.length) : null;

let messageNo = 0;
function message(fields: AnyRecord): AnyRecord {
  messageNo += 1;
  return { id: `m${messageNo}`, sessionId: A, timestamp: `2026-10-29T09:${String(messageNo % 60).padStart(2, '0')}:00.000Z`, provider: 'claude', ...fields };
}

const timelineA = new SessionTimeline(toRelative);
const history: AnyRecord[] = [
  message({ kind: 'text', role: 'user', content: 'Summarise the attached figures', images: [{ data: 'data:image/png;base64,iVBORw0KGgo=' }], files: [{ path: '/workspace/uploads/q3.xlsx', name: 'q3.xlsx', size: 2048 }] }),
  message({ kind: 'thinking', content: 'The sheet has three tabs.' }),
  message({ kind: 'tool_use', toolId: 't-bash', toolName: 'Bash', toolInput: { command: 'ls uploads', description: 'List uploads' } }),
  message({ kind: 'tool_result', toolId: 't-bash', content: 'q3.xlsx\n', toolResult: { content: 'q3.xlsx\n', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-read', toolName: 'Read', toolInput: { file_path: '/workspace/uploads/missing.txt' } }),
  message({ kind: 'tool_result', toolId: 't-read', content: 'File does not exist.', isError: true, toolResult: { content: 'File does not exist.', isError: true } }),
  message({ kind: 'tool_use', toolId: 't-edit', toolName: 'Edit', toolInput: { file_path: '/workspace/notes.md', old_string: 'a', new_string: 'b' } }),
  message({ kind: 'tool_result', toolId: 't-edit', content: 'The file has been updated.', toolResult: { content: 'The file has been updated.', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-search', toolName: 'mcp__yyc__search', toolInput: { query: 'hang seng index' } }),
  message({ kind: 'tool_result', toolId: 't-search', content: '[{"title":"HSI"}]', toolResult: { content: '[{"title":"HSI"}]', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-search2', toolName: 'mcp__tools__search', toolInput: { query: 'hang seng index' } }),
  message({ kind: 'tool_result', toolId: 't-search2', content: '[{"title":"HSI"}]', toolResult: { content: '[{"title":"HSI"}]', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-web', toolName: 'WebSearch', toolInput: { query: 'HKEX turnover October 2026' } }),
  message({ kind: 'tool_result', toolId: 't-web', content: 'Results…', toolResult: { content: 'Results…', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-todo', toolName: 'TodoWrite', toolInput: { todos: [{ content: 'Read the sheet', status: 'completed', activeForm: 'Reading the sheet' }, { content: 'Write the summary', status: 'in_progress', activeForm: 'Writing the summary' }] } }),
  message({ kind: 'tool_result', toolId: 't-todo', content: 'Todos have been modified successfully.', toolResult: { content: 'Todos have been modified successfully.', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-write', toolName: 'Write', toolInput: { file_path: '/workspace/summary.md', content: '# Summary\n' } }),
  message({ kind: 'tool_result', toolId: 't-write', content: 'File created successfully at: /workspace/summary.md', toolResult: { content: 'File created successfully at: /workspace/summary.md', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-send', toolName: 'SendUserFile', toolInput: { path: '/workspace/summary.md' } }),
  message({ kind: 'tool_result', toolId: 't-send', content: JSON.stringify({ path: '/workspace/summary.md', name: 'summary.md', size: 10, mimeType: 'text/markdown' }), toolResult: { content: JSON.stringify({ path: '/workspace/summary.md', name: 'summary.md', size: 10, mimeType: 'text/markdown' }), isError: false } }),
  message({ kind: 'tool_use', toolId: 't-task', toolName: 'Task', toolInput: { description: 'Check the totals', prompt: 'Add up column C', subagent_type: 'general-purpose' } }),
  message({ kind: 'tool_result', toolId: 't-task', content: 'Totals match.', toolResult: { content: 'Totals match.', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-glob', toolName: 'Glob', toolInput: { pattern: '**/*.xlsx' } }),
  message({ kind: 'tool_use', toolId: 't-grep', toolName: 'Grep', toolInput: { pattern: 'total', path: '/workspace' } }),
  message({ kind: 'tool_result', toolId: 't-grep', content: 'summary.md:3: total', toolResult: { content: 'summary.md:3: total', isError: false } }),
  message({ kind: 'tool_use', toolId: 't-fetch', toolName: 'WebFetch', toolInput: { url: 'https://www.example.com/report', prompt: 'Find the turnover' } }),
  message({ kind: 'tool_result', toolId: 't-fetch', content: 'Request failed with status 403', isError: true, toolResult: { content: 'Request failed with status 403', isError: true } }),
  message({ kind: 'text', role: 'assistant', content: '**Q3 revenue** rose 4%.\n\n| Tab | Rows |\n|---|---|\n| A | 10 |' }),
  message({ kind: 'error', content: 'The model provider returned an error. Try again.' }),
  message({ kind: 'text', role: 'user', content: '用中文再說一次' }),
  message({ kind: 'text', role: 'assistant', content: '第三季收入上升 4%。' }),
];
for (const m of history) timelineA.apply(m as never);
const timelineB = new SessionTimeline(toRelative);
const timelines = new Map<string, SessionTimeline>([[A, timelineA], [B, timelineB], [NEW, new SessionTimeline(toRelative)], [C, new SessionTimeline(toRelative)]]);
hub.timeline = async (id: string) => {
  let timeline = timelines.get(id);
  if (!timeline) timelines.set(id, (timeline = new SessionTimeline(toRelative)));
  return timeline;
};
// the hub's createConversation replaces the new conversation's timeline; keep ours in step
const realCreate = hub.createConversation.bind(hub);
hub.createConversation = async (...args: unknown[]) => {
  const id = await realCreate(...args);
  timelines.set(id, (hub as any).timelines.get(id));
  return id;
};

/** What the hub emits when CloudCLI streams a message of a running turn (paseo-hub.ts appendLive). */
function live(sessionId: string, fields: AnyRecord, turnId: string) {
  const timeline = timelines.get(sessionId)!;
  const changed = timeline.apply(message({ sessionId, ...fields }) as never, turnId);
  if (changed.length) paseoHub.emit('event', { type: 'timeline', sessionId, rows: changed, epoch: timeline.epoch, turnId });
}

// ------------------------------------------------------------------------------------------------------- the run

function connect(name: string, headers: Record<string, string> = {}) {
  const socket = new RecordingSocket(name);
  handlePaseoConnection(socket as never, { user: { id: 1 }, headers } as never, { runtime: {} } as never);
  let n = 0;
  return {
    socket,
    async request(body: AnyRecord, outsideSchema = false) {
      n += 1;
      socket.client({ type: 'session', message: { requestId: `${name}-${n}`, ...body } }, outsideSchema);
      await flush();
    },
    async push(body: AnyRecord, outsideSchema = false) {
      socket.client({ type: 'session', message: body }, outsideSchema);
      await flush();
    },
  };
}

/** The payload of the last frame of `type` the bridge sent. */
function lastPayload(type: string): AnyRecord | undefined {
  for (let i = lines.length - 1; i >= 0; i--) {
    const line = JSON.parse(lines[i]!) as AnyRecord;
    if (line.dir === 'out' && line.frame?.message?.type === type) return line.frame.message.payload;
  }
  return undefined;
}

const web = connect('web');

currentCase = 'handshake';
web.socket.client({ type: 'ping' });
web.socket.client({ type: 'session', message: { type: 'fetch_agents_request', requestId: 'before-hello' } });
web.socket.raw('{not json');
web.socket.client({ type: 'hello', clientId: 'cid-web-1', clientType: 'browser', protocolVersion: 1, appVersion: '0.13.2', capabilities: {} });
await flush();
web.socket.client({ type: 'ping' });
await flush();

currentCase = 'static and developer-machine requests';
for (const body of [
  { type: 'client_heartbeat', deviceType: 'web', focusedAgentId: null, lastActivityAt: '2026-10-29T09:00:00.000Z', appVisible: true },
  { type: 'session.events.set_subscription.request', events: ['project.update'] },
  { type: 'workspace.label.list.request', subscribe: {} },
  { type: 'workspace.label.list.request', sync: { generation: 'g', afterSeq: 0 } },
  { type: 'hosts.list.request' },
  { type: 'usage.list_reports.request' },
  { type: 'plugin.catalog.get.request' },
  { type: 'project.icon.get.request', projectId: 'prj_conversations' },
  { type: 'project_icon_request', cwd: '/workspace' },
  { type: 'list_provider_features_request', draftConfig: { provider: 'claude', cwd: '/workspace' } },
  { type: 'get_daemon_config_request' },
  { type: 'checkout_status_request', cwd: '/workspace' },
  { type: 'checkout_pr_status_request', cwd: '/workspace' },
  { type: 'subscribe_terminals_request', cwd: '/workspace', workspaceId: `wks_${A}` },
  { type: 'list_terminals_request', cwd: '/workspace', workspaceId: `wks_${A}` },
  { type: 'workspace.script.list.request', workspaceId: `wks_${A}` },
  { type: 'agent.provider_subagents.list.request', parentAgentId: A },
  { type: 'get_providers_snapshot_request', cwd: '/workspace' },
  { type: 'get_providers_snapshot_request' },
]) await web.request(body);

currentCase = 'conversation list';
await web.request({ type: 'project.list.request', subscribe: {}, sync: {} });
await web.request({ type: 'fetch_workspaces_request', subscribe: {}, sync: {} });
await web.request({ type: 'fetch_agents_request', subscribe: {}, sync: { generation: 'stale', headSeq: 3 } });
await web.request({ type: 'fetch_agent_history_request' });
await web.request({ type: 'fetch_agent_history_request', search: '舊' });
await web.request({ type: 'fetch_agent_request', agentId: A });
await web.request({ type: 'fetch_agent_request', agentId: C });
await web.request({ type: 'fetch_agent_request', agentId: MISSING });

currentCase = 'timeline';
await web.request({ type: 'agent.timeline.set_subscription.request', agentIds: [A, B, NEW] });
await web.request({ type: 'fetch_agent_timeline_request', agentId: A, direction: 'tail', limit: 5, projection: 'projected' });
await web.request({ type: 'fetch_agent_timeline_request', agentId: A, direction: 'before', cursor: { epoch: timelineA.epoch, seq: 20 }, limit: 5, projection: 'projected' });
await web.request({ type: 'fetch_agent_timeline_request', agentId: A, direction: 'after', cursor: { epoch: timelineA.epoch, seq: 3 }, limit: 500, projection: 'projected' });
await web.request({ type: 'fetch_agent_timeline_request', agentId: A, direction: 'after', cursor: { epoch: 'an-old-epoch', seq: 3 }, limit: 50, projection: 'projected' });
await web.request({ type: 'fetch_agent_timeline_request', agentId: B, direction: 'tail', limit: 50, projection: 'projected' });
await web.request({ type: 'fetch_agent_timeline_request', agentId: MISSING, direction: 'tail', limit: 50, projection: 'projected' });
await web.request({ type: 'agent.timeline.list_prompts.request', agentId: A });
await web.request({ type: 'agent.timeline.list_prompts.request', agentId: B });

currentCase = 'send and a live turn';
await web.request({ type: 'send_agent_message_request', agentId: A, text: 'And Q4?', messageId: 'client-msg-1', attachments: [{ type: 'uploaded_file', id: 'uploads/q4.pdf', fileName: 'q4.pdf', mimeType: 'application/pdf', size: 4096, path: 'uploads/q4.pdf' }] });
running.add(A);
paseoHub.emit('event', { type: 'preparing', sessionId: A, state: 'preparing' });
paseoHub.emit('event', { type: 'preparing', sessionId: A, state: 'ready' });
const turn = 'turn-live';
live(A, { kind: 'thinking', content: 'Q4 is in the second file.' }, turn);
live(A, { kind: 'tool_use', toolId: 'live-read', toolName: 'Read', toolInput: { file_path: '/workspace/uploads/q4.pdf' } }, turn);
live(A, { kind: 'tool_result', toolId: 'live-read', content: '…', toolResult: { content: '…', isError: false } }, turn);
live(A, { kind: 'text', role: 'assistant', content: 'Q4 revenue fell 1%.' }, turn);
await flush();
await web.request({ type: 'send_agent_message_request', agentId: A, text: 'busy?', messageId: 'client-msg-2' });
await web.request({ type: 'fetch_agent_request', agentId: A });
running.delete(A);
paseoHub.emit('event', { type: 'turn', sessionId: A, phase: 'completed', turnId: turn });
await flush();

currentCase = 'failed and stopped turns';
await web.request({ type: 'send_agent_message_request', agentId: B, text: 'Hello', messageId: 'client-msg-3' });
paseoHub.emit('event', { type: 'turn', sessionId: B, phase: 'failed', turnId: 'turn-b1', error: 'The model provider returned an error.' });
await flush();
running.add(B);
await web.request({ type: 'cancel_agent_request', agentId: B });
running.delete(B);
paseoHub.emit('event', { type: 'turn', sessionId: B, phase: 'canceled', turnId: 'turn-b2' });
await flush();
await web.request({ type: 'cancel_agent_request', agentId: B });

currentCase = 'new conversation';
await web.request({ type: 'workspace.create.request', idempotencyKey: 'idem-1', subscribe: true, source: { kind: 'directory', path: '/workspace', projectId: 'prj_conversations' }, agent: { config: { provider: 'claude', cwd: '/workspace' }, initialPrompt: 'Draft a reply to the auditor', clientMessageId: 'client-msg-4' } });
await web.request({ type: 'workspace.create.request', idempotencyKey: 'idem-1', source: { kind: 'directory', path: '/workspace', projectId: 'prj_conversations' }, agent: { config: { provider: 'claude', cwd: '/workspace' }, initialPrompt: 'Draft a reply to the auditor', clientMessageId: 'client-msg-4' } });

currentCase = 'rename';
await web.request({ type: 'update_agent_request', agentId: B, title: 'Greetings' });
await web.request({ type: 'update_agent_request', agentId: MISSING, title: 'Nobody' });
await web.request({ type: 'workspace.title.set.request', workspaceId: `wks_${B}`, title: '問候' });
await web.request({ type: 'workspace.title.set.request', workspaceId: `wks_${B}`, title: '   ' });
paseoHub.emit('event', { type: 'session', sessionId: B });
await flush();

currentCase = 'delete, restore, delete for good';
await web.request({ type: 'archive_agent_request', agentId: B });
paseoHub.emit('event', { type: 'session', sessionId: B, removed: true });
await flush();
await web.request({ type: 'refresh_agent_request', agentId: B });
await web.request({ type: 'archive_workspace_request', workspaceId: `wks_${B}` });
await web.request({ type: 'delete_agent_request', agentId: B });
await web.request({ type: 'delete_agent_request', agentId: MISSING });
await web.request({ type: 'archive_workspace_request', workspaceId: `wks_${MISSING}` });

currentCase = 'unknown and released';
await web.request({ type: 'something_this_server_has_not.request' }, true);
await web.push({ type: 'something_without_a_request_id' }, true);
await web.request({ type: 'subscription.release.request', subscriptionId: lastPayload('fetch_workspaces_response')?.subscriptionId ?? 'none' });

// ------------------------------------------------------------------------------- an admin viewing as the person
const viewing = connect('view-as', { 'x-jarvis-user': 'person@example.com', 'x-jarvis-viewer': 'admin@example.com', 'x-jarvis-viewer-session': 'view-1' });
currentCase = 'view-as handshake';
viewing.socket.client({ type: 'hello', clientId: 'cid-admin-1', clientType: 'browser', protocolVersion: 1, appVersion: '0.13.2', capabilities: {} });
await flush();
currentCase = 'view-as refusals';
await viewing.request({ type: 'update_agent_request', agentId: A, title: 'x' });
await viewing.request({ type: 'workspace.title.set.request', workspaceId: `wks_${A}`, title: 'x' });
await viewing.request({ type: 'archive_agent_request', agentId: A });
await viewing.request({ type: 'archive_workspace_request', workspaceId: `wks_${A}` });
await viewing.request({ type: 'delete_agent_request', agentId: A });
await viewing.request({ type: 'refresh_agent_request', agentId: C });
currentCase = 'view-as sends';
await viewing.request({ type: 'send_agent_message_request', agentId: A, text: 'checking', messageId: 'client-msg-5' });
webOk = false;
await viewing.request({ type: 'send_agent_message_request', agentId: A, text: 'not recorded', messageId: 'client-msg-6' });
rows.delete(NEW);
await viewing.request({ type: 'workspace.create.request', idempotencyKey: 'idem-2', source: { kind: 'directory', path: '/workspace', projectId: 'prj_conversations' }, agent: { config: { provider: 'claude', cwd: '/workspace' }, initialPrompt: 'not recorded' } });
webOk = true;

// --------------------------------------------------------------------------------------------- an iOS-style client
const ios = connect('ios');
currentCase = 'ios handshake and list';
ios.socket.client({ type: 'hello', clientId: 'cid-ios-1', clientType: 'mobile', protocolVersion: 1, appVersion: '0.13.2', capabilities: {} });
await flush();
await ios.request({ type: 'fetch_workspaces_request', subscribe: {} });
rows.clear();
await ios.request({ type: 'fetch_workspaces_request' });
await ios.request({ type: 'fetch_agents_request' });

writeFileSync(out, `${lines.join('\n')}\n`);
console.log(`recorded ${lines.length} frames to ${out}`);
process.exit(0);

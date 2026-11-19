// The sample content the stand-in (stand-in.mjs) answers with: a person's conversation list and conversations that
// between them show every kind of row, chip, panel and question the app draws. Celebrities, never invented names.

const T0 = Date.parse("2026-11-18T10:00:00.000Z");
const iso = (min) => new Date(T0 + min * 60000).toISOString();
const project = (name) => ({ checkout: { currentBranch: null, cwd: "/workspace", isGit: false, isPaseoOwnedWorktree: false, mainRepoRoot: null, remoteUrl: null, worktreeRoot: null }, projectKey: "prj_conversations", projectName: "YYC AI", workspaceName: name });
/** Conversation 6 runs a turn while cards.mjs pushes its question card (the bug showed in a running turn). */
const running = (c) => c.running || (options.cardsLive && c.n === 6);
const CONVS = [
  { n: 1, title: "季度銷售報告", min: 50 },
  { n: 2, title: "活動海報文案", min: 40, running: true },
  { n: 3, title: "每朝新聞摘要", min: 30, schedule: true },
  { n: 4, title: "客戶電郵草稿", min: 20, background: 2 },
  { n: 5, title: "九龍灣倉庫租約比較", min: 10, attention: true },
  { n: 6, title: "員工旅行行程", min: -60 },
  { n: 7, title: "新產品命名諗法", min: -120 },
  { n: 8, title: "會議紀錄整理", min: -200 },
  { n: 9, title: "網站文字翻譯", min: -300 },
];
export const wid = (n) => `wks_0000000${n}-0000-4000-8000-000000000000`;
export const aid = (n) => `0000000${n}-0000-4000-8000-00000000000a`;
function agent(c) {
  return { activeTurn: running(c) || c.attention ? { startedAt: iso(c.min), turnId: `turn-${c.n}-r` } : null, archivedAt: null,
    attentionReason: c.attention ? "finished" : null, attentionTimestamp: c.attention ? iso(c.min) : null, availableModes: [],
    capabilities: { supportsDynamicModes: false, supportsMcpServers: false, supportsReasoningStream: true, supportsRewindBoth: false, supportsRewindConversation: false, supportsRewindFiles: false, supportsSessionListing: false, supportsSessionPersistence: true, supportsStreaming: true, supportsToolInvocations: true },
    closed: null, createdAt: iso(c.min - 5), currentModeId: null, cwd: "/workspace", effectiveThinkingOptionId: "high", features: [], id: aid(c.n), labels: {},
    lastUserMessageAt: iso(c.min), model: "deepseek/deepseek-v4.1-flash-1017", pendingMessages: [], pendingPermissions: c.attention ? [ASK] : [], persistence: null, provider: "claude",
    requiresAttention: !!c.attention, runtimeInfo: { modeId: null, model: "deepseek/deepseek-v4.1-flash-1017", provider: "claude", sessionId: aid(c.n) },
    status: running(c) || c.attention ? "running" : "idle", thinkingOptionId: "high", title: c.title, updatedAt: iso(c.min), workspaceId: wid(c.n) };
}
function workspace(c) {
  return { activityAt: iso(c.min), archivingAt: null, background: false, backgroundTasks: c.background || 0, createdAt: iso(c.min - 5), diffStat: null, id: wid(c.n),
    lastUserMessageAt: iso(c.min), name: c.title, nextScheduleAt: c.schedule ? iso(24 * 60) : null, pinnedAt: null, project: project(c.title),
    projectCustomIconRevision: null, projectCustomName: null, projectDisplayName: "YYC AI", projectId: "prj_conversations", projectKind: "directory", projectRootPath: "/workspace",
    scripts: [], setupStatus: null, status: running(c) ? "running" : "done", statusEnteredAt: iso(c.min), title: c.title, workspaceDirectory: "/workspace", workspaceKind: "directory" };
}

// the rich conversation (n = 1)
const reply1 = `## 第三季銷售摘要

整體營業額 **HK$4,820 萬**，比上季高 12%。主要來自網店同九龍灣門市。

| 地區 | 營業額 | 按季 |
|---|---|---|
| 港島 | 1,240 萬 | +8% |
| 九龍 | 2,110 萬 | +15% |
| 新界 | 1,470 萬 | +11% |

重點：
1. 九月嘅中秋禮盒賣得最好，佔季度 18%。
2. 網店訂單多咗 2,300 張。
3. 新界區退貨率偏高（4.1%），值得再睇。

計算用嘅程式碼：

\`\`\`python
import pandas as pd
df = pd.read_excel("uploads/q3.xlsx")
print(df.groupby("region")["sales"].sum())
\`\`\`

資料來源：[公司銷售系統](https://example.com/sales)。`;
let seq = 0;
const e = (item, turnId, min, collapsed = []) => { seq += 1; return { collapsed, item, provider: "claude", seqEnd: seq, seqStart: seq, sourceSeqRanges: [{ endSeq: seq, startSeq: seq }], timestamp: iso(min), ...(turnId ? { turnId } : {}) }; };
const tool = (callId, name, detail, extra = {}) => ({ callId, detail, error: null, name, status: "completed", type: "tool_call", ...extra });
function richTimeline() {
  seq = 0;
  return [
    e({ attachments: [{ fileName: "q3.xlsx", id: "uploads/u/q3.xlsx", mimeType: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", path: "uploads/u/q3.xlsx", size: 48200, type: "uploaded_file" }],
        clientMessageId: "cm-1", images: [{ mimeType: "image/png", thumbnailUrl: "/api/blobs/chart?v=display", url: "/api/blobs/chart?v=display" }], messageId: "cm-1",
        text: "幫我睇吓呢份第三季銷售表，同上季比較，再整一份 PDF 報告。", type: "user_message" }, null, 0),
    e({ text: "先讀表，再計各區總數。", type: "reasoning" }, "t1", 0.1),
    e(tool("toolu_b", "Bash", { command: "ls uploads", output: "q3.xlsx\n", type: "shell" }), "t1", 0.2, ["tool_lifecycle"]),
    e(tool("toolu_r", "Read", { content: "region,sales\n港島,12400000", filePath: "/workspace/uploads/q3.xlsx", offset: 1, type: "read" }), "t1", 0.3, ["tool_lifecycle"]),
    e(tool("toolu_f", "WebFetch", { prompt: "上季數字", type: "fetch", url: "https://example.com/sales" }), "t1", 0.4, ["tool_lifecycle"]),
    e(tool("toolu_k", "Skill", { icon: "sparkles", label: "report", text: "loaded", type: "plain_text" }), "t1", 0.5, ["tool_lifecycle"]),
    e(tool("toolu_w", "Write", { content: "# 報告", filePath: "/workspace/report.html", type: "write" }), "t1", 0.6, ["tool_lifecycle"]),
    e({ messageId: "msg_1", text: reply1, type: "assistant_message" }, "t1", 0.7),
    e(tool("toolu_s", "SendUserFile", { input: { path: "/workspace/第三季銷售報告.pdf" }, output: "sent", type: "unknown" },
        { metadata: { sentCaption: "第三季銷售報告", sentFiles: [{ isImage: false, mediaType: "application/pdf", name: "第三季銷售報告.pdf", path: "第三季銷售報告.pdf", sent: "toolu_s", size: 284000 }, { isImage: true, mediaType: "image/png", name: "各區營業額.png", path: "各區營業額.png", sent: "toolu_s", size: 54000 }] } }), "t1", 0.8, ["tool_lifecycle"]),
    e({ messageId: "msg_2", text: "報告已經整好，共 7 頁，附咗各區營業額嘅圖。", type: "assistant_message" }, "t1", 1.2),
    e({ clientMessageId: "cm-2", messageId: "cm-2", text: "新界退貨率高係咩原因？", type: "user_message" }, null, 3),
    e(tool("toolu_x", "Read", { content: "", filePath: "/workspace/uploads/returns.xlsx", type: "read" }, { error: { message: "File does not exist." }, status: "failed" }), "t2", 3.1, ["tool_lifecycle"]),
    e({ messageId: "msg_3", text: "表入面冇退貨原因嘅欄位。如果你有退貨紀錄，上載之後我可以再分析。", type: "assistant_message" }, "t2", 3.3),
  ];
}
const CHART = '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="500" viewBox="0 0 800 500"><rect width="800" height="500" fill="#fff"/><rect x="120" y="210" width="140" height="240" fill="#0A7F95"/><rect x="340" y="50" width="140" height="400" fill="#e0a54a"/><rect x="560" y="150" width="140" height="300" fill="#5aa9ff"/></svg>';

const ASK = { id: "ask-1", kind: "question", name: "AskUserQuestion", provider: "claude", title: "揀一個方案",
  input: { questions: [{ question: "你想比較邊兩個地點嘅租約？", header: "地點", multiSelect: false, allowOther: true,
    options: [{ label: "九龍灣同觀塘", description: "兩個都近港鐵，租金差 8%" }, { label: "九龍灣同葵涌", description: "葵涌倉庫大啲，但遠啲" }, { label: "三個都比較", description: "會耐少少" }] }] } };
const TASKS = [
  { taskId: "t1", type: "command", description: "下載新聞網站嘅頭條", command: "python fetch.py --all", status: "running", startedAt: iso(15), now: "已下載 34 / 80 頁", steps: 34 },
  { taskId: "t2", type: "subagent", description: "比較三間供應商報價", status: "running", startedAt: iso(17), now: "讀緊第二份報價單", steps: 6 },
  { taskId: "t3", type: "command", description: "轉換影片格式", command: "ffmpeg -i a.mov a.mp4", status: "completed", startedAt: iso(5), endedAt: iso(9), summary: "完成，檔案 48 MB" },
];
const SCHEDULES = [
  { id: "s1", conversationId: aid(3), kind: "cron", prompt: "整理今日香港財經新聞頭條，五點以內。", cron: "30 8 * * 1-5", tz: "Asia/Hong_Kong", describe: "每個工作日 08:30", next: iso(22 * 60 + 30), state: "active", createdBy: "Jackie", createdAt: iso(-3000), runs: [{ due: iso(-90), at: iso(-90), outcome: "sent" }, { due: iso(-1530), at: iso(-1530), outcome: "sent" }] },
  { id: "s2", conversationId: aid(3), kind: "once", prompt: "提醒我交季度報告。", at: iso(3 * 24 * 60), tz: "Asia/Hong_Kong", describe: "11 月 21 日 18:00", next: iso(3 * 24 * 60), state: "active", createdBy: "Jackie", createdAt: iso(-100), runs: [] },
];
export function status(s) { s.payload.hostname = "YYC AI"; return s; }
/** What a check turns on: `personCards` (the deployment's switch), `cardsLive` (conversation 6 gets the live pushes
 * of a question card, cards.mjs). */
export const options = { personCards: false, cardsLive: false };

// The sequence that drew one question twice (Deyao, 2026-11-19; session "Fix CI failure blocking YYC AI system prompt
// deploy"): the `ask` call runs in the turn, the question's card row opens with no turn and is answered, then the
// `ask` call completes (an update of an earlier row: the app put its turn on the card, the last item), and the app
// loads the timeline again (here a replacement; a reconnect or a scroll does the same), whose card row has no turn.
const CARD_INPUT = { questions: [{ question: "點樣入 main？", header: "入 main", multiSelect: false, options: [{ label: "而家合併" }, { label: "等 gate" }] }] };
const cardOf = (state) => ({ kind: "question", id: "q-6", marker: "(answer to q-6)", state, askedAt: iso(-59), input: CARD_INPUT,
  ...(state === "open" ? {} : { answeredAt: iso(-58), answer: { behavior: "allow", updatedInput: { answers: { "點樣入 main？": "而家合併" } } } }) });
const cardItem = (state) => ({ type: "tool_call", callId: "q-6", name: "ask", status: state === "open" ? "running" : "completed", error: null,
  detail: { type: "unknown", input: CARD_INPUT, output: null }, metadata: { personAsk: cardOf(state) } });
const askItem = (status) => ({ type: "tool_call", callId: "toolu_6", name: "Bash", status, error: null,
  detail: { type: "shell", command: "ask < q.json", ...(status === "completed" ? { output: "Question q-6 is open; waiting for the answer.\n→ 而家合併" } : {}) } });
export const cards = { reloaded: false };
function cardPushes(agentId, send) {
  const push = (item, turnId, seq) => ({ type: "agent_stream", payload: { subscriptionId: "00000000-0000-4000-8000-000000000000", agentId,
    event: { type: "timeline", provider: "claude", ...(turnId ? { turnId } : {}), item }, timestamp: iso(-58), seq, epoch: "e1" } });
  const frames = [push(askItem("running"), "turn-6-r", 3), push(cardItem("open"), null, 4), push(cardItem("answered"), null, 5),
    push(askItem("completed"), "turn-6-r", 6)];
  frames.forEach((frame, i) => setTimeout(() => send(frame), 600 + i * 250));
  setTimeout(() => {
    cards.reloaded = true;
    send({ type: "agent.timeline.replacement", payload: { subscriptionId: "00000000-0000-4000-8000-000000000000", agentId, epoch: "e2" } });
  }, 600 + frames.length * 250 + 500);
}
// conversation 6's timeline after the reload: the same rows as pushed, the card's with no turn
function reloadedTimeline(c) {
  seq = 0;
  return [
    e({ clientMessageId: `cm-${c.n}`, messageId: `cm-${c.n}`, text: `關於「${c.title}」，幫我整理一下。`, type: "user_message" }, null, c.min),
    e({ text: "整理緊…", type: "reasoning" }, `turn-${c.n}-r`, c.min + 0.1),
    e(askItem("completed"), `turn-${c.n}-r`, c.min + 0.2),
    e(cardItem("answered"), null, c.min + 0.3),
  ];
}

export function rest(method, p) {
  if (p === "/api/jarvis/me") return { firstName: "Jackie", sharing: true, newConversations: true, conversationDelete: true, product: "YYC AI", midTurn: true, modelPick: true, personCards: options.personCards };
  if (p.startsWith("/api/blobs/") || p.includes("/files/raw")) return { svg: CHART };
  if (/\/api\/jarvis\/sessions\/[^/]+\/tasks$/.test(p)) return { tasks: p.includes("/00000004-") ? TASKS : [] }; // the session id is the workspace's uuid
  if (/\/api\/jarvis\/sessions\/[^/]+\/schedules$/.test(p)) return { schedules: p.includes("/00000003-") ? SCHEDULES : [] };
  if (/\/api\/jarvis\/sessions\/[^/]+\/shares$/.test(p)) return method === "POST" ? { id: "sh2", url: "/s/sh2", createdAt: iso(60) } : { shares: [{ id: "sh1", url: "/s/sh1", createdAt: iso(30) }] };
  if (/^\/api\/jarvis\/shares\/[^/]+$/.test(p)) return { id: "sh1", title: "季度銷售報告", ownerName: "Jackie", isOwner: false, createdAt: iso(30), timeline: richTimeline().map(({ collapsed, sourceSeqRanges, ...r }) => r) };
  if (p === "/api/jarvis/warm") return {};
  return undefined;
}
// MiMo runs in another harness than DeepSeek, so picking it in a conversation with messages asks first
const SNAPSHOT_MODELS = [
  { description: "", id: "deepseek/deepseek-v4.1-flash-1017", isDefault: true, label: "DeepSeek V4.1 Flash", thinkingSet: 0, metadata: { harness: "qwen" } },
  { description: "", id: "xiaomi/mimo-v2.6-pro", isDefault: false, label: "MiMo v2.6 Pro", thinkingSet: 1 },
];
/** Answers the request `m` from `tpl` (pigeon's recorded answer of its type) with this content; false: not one of these. */
export function answer(m, send, tpl) {
  const p = (payload) => (send({ type: tpl.type, payload: { ...tpl.payload, ...payload, requestId: m.requestId } }), true);
  switch (m.type) {
    case "client_heartbeat": return true;
    case "send_agent_message_request": return true; // never confirmed: the outbox keeps it
    case "fetch_workspaces_request": return p({ entries: CONVS.map(workspace), pageInfo: { hasMore: false, nextCursor: null, prevCursor: null }, emptyProjects: [], subscriptionId: m.subscriptionId ?? tpl.payload.subscriptionId, sync: { generation: "gen-1", headSeq: 1, mode: "snapshot", reason: "no_cursor", removals: [] } });
    case "fetch_agents_request": return p({ entries: CONVS.map((c) => ({ agent: agent(c), project: project(c.title) })), pageInfo: { hasMore: false, nextCursor: null, prevCursor: null }, sync: { generation: "gen-1", headSeq: 1, mode: "snapshot", reason: "no_cursor", removals: [] } });
    case "fetch_agent_request": { const c = CONVS.find((c) => aid(c.n) === m.agentId) ?? CONVS[0]; return p({ agent: agent(c), project: project(c.title), error: null }); }
    case "fetch_agent_timeline_request": {
      const c = CONVS.find((c) => aid(c.n) === m.agentId) ?? CONVS[0];
      const entries = options.cardsLive && c.n === 6 && cards.reloaded ? reloadedTimeline(c) : c.n === 1 ? richTimeline() : (seq = 0, [e({ clientMessageId: `cm-${c.n}`, messageId: `cm-${c.n}`, text: `關於「${c.title}」，幫我整理一下。`, type: "user_message" }, null, c.min), ...(running(c) ? [e({ text: "整理緊…", type: "reasoning" }, `turn-${c.n}-r`, c.min + 0.1)] : [e({ messageId: `m-${c.n}`, text: "好，以下係整理好嘅重點：\n\n- 第一點\n- 第二點\n- 第三點", type: "assistant_message" }, `turn-${c.n}`, c.min + 0.5)])]);
      if (c.attention) setTimeout(() => send({ type: "agent_permission_request", payload: { agentId: aid(c.n), request: ASK } }), 300);
      const ep = options.cardsLive && c.n === 6 && cards.reloaded ? "e2" : "e1";
      if (options.cardsLive && c.n === 6 && !cards.reloaded) cardPushes(aid(c.n), send);
      return p({ agent: agent(c), agentId: m.agentId, entries, direction: "after", startCursor: { epoch: ep, seq: 1 }, endCursor: { epoch: ep, seq }, epoch: ep, window: { maxSeq: seq, minSeq: 1, nextSeq: seq + 1 }, hasNewer: false, hasOlder: false, gap: false, reset: true, staleCursor: false, error: null });
    }
    case "get_providers_snapshot_request": {
      const snap = { entries: [{ defaultModeId: null, description: "", enabled: true, label: "YYC AI", models: SNAPSHOT_MODELS, modes: [], provider: "claude", source: "builtin", status: "ready" }],
        thinkingSets: [{ defaultOptionId: "high", options: ["none", "low", "high", "max"].map((id) => ({ id, isDefault: id === "high", label: id })) }, { defaultOptionId: "high", options: ["low", "high"].map((id) => ({ id, isDefault: id === "high", label: id })) }] };
      return p({ compactSnapshot: snap, snapshotHash: "fixture-1" });
    }
    case "list_commands_request": return p({ agentId: m.agentId, commands: [{ argumentHint: "<instructions>", description: "總結對話，騰出空間", kind: "command", name: "compact" }, { argumentHint: "", description: "整一份報告", kind: "skill", name: "report" }], error: null });
    case "agent.timeline.list_prompts.request": return p({ agentId: m.agentId, prompts: [] });
    // the agents the app asked for (the recorded answer names another): their live pushes reach the app
    case "agent.timeline.set_subscription.request": return p({ agentIds: m.agentIds ?? [], subscriptionId: m.subscriptionId ?? tpl.payload.subscriptionId });
  }
  return false;
}

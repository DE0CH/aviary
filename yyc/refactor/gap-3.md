# Gap 3: notifications, attention and presence as staff see them

Research only (2026-11-02). Sources: DE0CH/claudecodeui at 3de3f5d8 (the live `ui` image), DE0CH/jarvis3 at 3f7499a,
the inventory, and read-only checks on YYC's cluster (database `cloudcli` on `db/pg-1`, `jarvis` kv). Paths:
`app/` = `claudecodeui/paseo/packages/app/`, `client/` = `claudecodeui/paseo/packages/client/src/`, `bridge/` =
`claudecodeui/server/modules/paseo/`, `server/` = `claudecodeui/server/`, `router.mjs` = `claudecodeui/jarvis/router.mjs`.
"code-derived" = worked out from code, not watched live.

## Verdict in one paragraph

A staff member who leaves a conversation is told nothing until they look again. There is no web push, no browser
Notification, no iOS notification, no badge, no sound and no unread dot in production. All of that code exists (in
the Paseo app and in CloudCLI's server), but nothing ever triggers it. What staff actually get is in-app state they can
see when they look: the running loader on the conversation row, the running favicon in the browser tab, the stop
button and streaming in the open conversation, the 「準備工作空間」 (preparing) chip, the list re-sorting by activity, live
title changes (provisional name, then the generated title, then renames from other devices), and the same conversation
updating live on a second tab or device. Separately, typing warms an AWS host. All of these are live and must be kept.
The attention/unread/notification/presence machinery is dead and should be dropped in the cut. One gap is real and
nobody can see it: when Claude calls AskUserQuestion or ExitPlanMode, the conversation keeps spinning forever, because
the ask is never shown.

---

## 1. Signal table

| # | Signal as staff see it | Live? | Source today (file:line) | Target home |
|---|---|---|---|---|
| 1 | Conversation row shows a running loader while a turn runs | **live, keep** | bridge `paseo-connection.ts:560,577` (workspace `status: running/done`), `:589,603-604` (agent `status running/idle`, `activeTurn`); `paseo-hub.ts:179-188` (`isRunning` = `chatRunRegistry.isProcessing`); registry `chat-run-registry.service.ts:263-265` | magpie (turn state per conversation, in the events + conversation row) → pigeon maps it to `status`/`activeTurn` |
| 2 | Browser tab favicon switches to the "running" image while any conversation runs | **live, keep** | `app/src/hooks/use-favicon-status.ts:14-18,25-38,114-125`; mounted `app/src/app/_layout.tsx:758`; images `app/assets/images/favicon{,-running,-attention}.png` | apps (unchanged); fed by #1 |
| 3 | Favicon "attention" image | dead (never `requiresAttention`, never pending permissions) | `use-favicon-status.ts:32-35`; bridge `paseo-connection.ts:609,614-616` | apps keep the code; magpie/pigeon only if #9 is built |
| 4 | Open conversation streams, shows stop, ends with done/failed/stopped | **live, keep** | bridge `paseo-connection.ts:789-807` (`turn_started/completed/failed/canceled` as `agent_stream`), hub `paseo-hub.ts:290-291,342-347,358-370`; app `contexts/session-context.tsx:360-384`, `timeline/session-stream-reducers.ts:1810-1829` | supervisor (source) → magpie (event rows with explicit turn boundaries) → pigeon |
| 5 | 「準備工作空間」 chip while the sandbox is made | **live, keep** | `server/modules/providers/list/claude/claude-runtime.provider.js:1093-1094` (`status` frame with `jarvisSandbox`); hub `paseo-hub.ts:349-357,366`; bridge `paseo-connection.ts:811-814` (`yyc.sandbox_status`, to every connection); app `src/yyc/sandbox-status.ts:38`, `src/yyc/sandbox-chip.tsx` | hermit says when the sandbox is ready → magpie records "preparing/ready" as an event → pigeon sends `yyc.sandbox_status` |
| 6 | Conversation list re-sorts: a run start and end move it to the top | **live, keep** | `chat-run-registry.service.ts:121-122,183-193,255` (`touchSession` + `broadcastSessionUpserted` at start and `complete`); bridge `workspaceDescriptor` `activityAt/statusEnteredAt = updated_at` `paseo-connection.ts:578-581` | magpie (conversation `updated_at` + a list-change event) |
| 7 | Title changes live: provisional name → generated title → renames from any device; the browser tab title follows | **live, keep** | provisional `sessions.service.ts:139-145,343-347`; generated `jarvis-title.ts:6-13,129-151`, triggered `claude-runtime.provider.js:1104-1109`, saved `sessions.db.ts:415-419`; rename `sessions.service.ts:909-920`, `sessions.db.ts:400-404` (`name_source='user'` wins); provisional guard `sessions.db.ts:407-412` and transcript upsert `:116-119`; push via `broadcastSessionUpserted` → hub `paseo-hub.ts:319-325` → bridge `pushSession` `paseo-connection.ts:655-697`; tab title `app/src/screens/workspace/workspace-screen.tsx:320-337,2904-2909`; shell `<title>` from product name `router.mjs:632,1115-1117` | magpie (title + `name_source` + title job; list-change event); the title-model call goes through `model` (egress) as today |
| 8 | Second tab/device of the same person follows a run started elsewhere | **live, keep** | hub `paseo-hub.ts:220-224` (opened mid-run: subscribe), `:319-323` (`session_upserted` while running: follow); per-person process shared by all tabs `paseo-hub.ts:1-9,131-136`; doc `claudecodeui/docs/paseo-ui.md:56-57` | magpie events by seq (any pigeon replica tails `GET …/events?after=`); no follow logic needed |
| 9 | Unread / "needs attention" dot after a reply finished while away; "Mark as read/unread" | dead | bridge always `requiresAttention:false, attentionReason:null, attentionTimestamp:null` `paseo-connection.ts:614-616`, workspace status only `running/done` `:577`; `clear_agent_attention` and `workspace.clear_attention/mark_unread` fall to `rpc_error unsupported` `:501-510`; menu hidden: `app/src/hooks/use-workspace-read-state.ts:22-25` (needs status `attention/failed`, or feature `workspaceMarkUnread`, absent from `FEATURES` `paseo-connection.ts:40-56`); `components/sidebar-workspace-list.tsx:1234-1248,1285-1286`; clear hook `app/src/hooks/use-agent-attention-clear.ts:43-63` (only when `requiresAttention`) | drop for the cut (app code stays inert). A new feature if wanted (section 4) |
| 10 | Red "failed" dot on a conversation whose turn failed while away | dead | bucket `failed` needs `attentionReason:'error'` (`paseo/packages/protocol/src/agent-state-bucket.test.ts:31-40`); never sent | same as #9 |
| 11 | Browser Notification ("Claude finished" / "needs permission") | dead | `app/src/contexts/session-context.tsx:257-310` (`notifyAgentAttention`), fed only by `agent_attention_required` `:485-515`; the bridge never sends that message (no occurrence in `bridge/`); `session.events.set_subscription.request` gets an id and no events `paseo-connection.ts:210-212`; permission only requested in Electron `app/src/app/_layout.tsx:806-809`; sender `app/src/utils/os-notifications.ts:144-172` | drop for the cut |
| 12 | Terminal attention notification | dead (no terminals: `subscribe_terminals_request` → empty `paseo-connection.ts:286-298`) | `session-context.tsx:662-681` | drop |
| 13 | iOS notification / badge / sound | dead, deliberately | `app/app.config.js:129-136` (comment "No QR pairing and no push (one server, no APNs key)"; `notificationPlugins` not spread into plugins, `:45-53` vs `:169`); `app/plugins/with-no-push-entitlement.js:3-9` (strips `aps-environment`); `app/src/push-notifications/index.ts:3-9` (no token, no prompt); native local notifications refused `os-notifications.ts:145-148`; handler with `shouldPlaySound:false, shouldSetBadge:false` `app/src/notification-open/source.native.ts:12-20`; `expo-notifications` still a dependency `app/package.json:100`; doc `docs/paseo-ui.md:317` | apps (stays off) |
| 14 | `client_heartbeat` presence (focused conversation, app visible, every 15 s) | dead on the server; **side effect live** | client `app/src/hooks/use-client-activity.ts:115-154`, `client-activity-tracker.ts:1` (15 s), `client/daemon-client.ts:2431-2448`; bridge ignores it `paseo-connection.ts:201-204`. Side effect: any socket data touches the per-person process `router.mjs:1200-1205`, so an open tab keeps that process alive past `JARVIS_UI_IDLE_MINUTES` 30 `router.mjs:74,932-940` | drop (pigeon is stateless; nothing to keep alive). Keep it ignored, don't route it anywhere |
| 15 | Push token registration | dead | `client/daemon-client.ts:2450-2455` (`register_push_token`, no requestId → silently dropped by the default branch `paseo-connection.ts:501-510`); app never calls it (`push-notifications/index.ts:3-5`) | drop |
| 16 | CloudCLI server Web Push / desktop notifications | dead | VAPID configured at start `server/index.ts:339-340`, `server/modules/notifications/vapid-keys.service.ts:8-30`; channels `notification-orchestrator.service.js:211-249`, events `:251-302`; call sites `claude-runtime.provider.js:982-987,1117-1135` (SDK Notification hook), `:1169-1178` (permission), `:1371,1382,1443,1508` (stop, background done, failed); routes `server/modules/settings/settings.routes.ts:41-49`; `/desktop-notifications` WS `websocket-server.service.ts:116-119`; defaults `database/repositories/notification-preferences.ts:24-36` (all channels off) | drop, with the tables (section 3) |
| 17 | "Needs your answer" (AskUserQuestion / ExitPlanMode) | **missing, and it hangs** | `claude-runtime.provider.js:93,1144-1146,1167-1181` (sends `permission_request`, waits with `timeoutMs 0` = forever, `:164-170`); hub drops it `paseo-hub.ts:335-337`; bridge `pendingPermissions: []` `paseo-connection.ts:609`; neither tool is disallowed anywhere in jarvis3, YYC or the fork's Jarvis modules | supervisor + magpie + pigeon must decide (section 5) |
| 18 | Typing warms a host | **live, keep** | app `src/yyc/warm.ts:9-25`, `composer/index.tsx:1317-1324`; server `server/modules/jarvis/jarvis-warm.routes.ts:15-60`; web `jarvis3/chat/src/web/app.ts:88-106`, `sandbox.ts:49-64` (`DEMAND_WINDOW_S` 120), scaler `sandbox-vm.ts:254-261,647`; live kv row `sandbox_demand:*` at 2026-11-02T03:46Z | pigeon (`POST /api/jarvis/warm` → hermit `warm`); hermit's YYC implementation keeps the demand hint |
| 19 | Scheduled messages ("a scheduled turn" in the hub comment) | dead | dispatcher still starts `server/index.ts:376-378`; route kept `server/index.ts:180`, guard comment `jarvis-guard.ts:11`; no Paseo caller; 0 rows in every `cc_*.scheduled_messages` (live) | drop |

---

## 2. What staff see, situation by situation

### 2.1 A reply finishes while they look at another conversation (same tab)
- Its row's loader stops (#1). The list moves it to the top (#6). No dot, no toast, no sound (#9, #11).
- The favicon goes back to plain once nothing runs (#2).
- If the turn **failed**, the row looks the same as a finished one: the bridge only reports `idle`
  (`paseo-connection.ts:603`); the error only shows inside the conversation (the hub's `turn_failed` and an error
  row, `paseo-hub.ts:342-347`, `:367-368`). `turn_*` stream events only reach connections whose
  `timelineAgents` include that conversation (`paseo-connection.ts:766,780`); everyone else gets an `agent_update`
  through `pushSession` (`:805`).

### 2.2 The tab is in the background, or the browser is minimised
- Same as 2.1, plus the favicon: the tab strip shows the running icon until the last run ends (#2). That is the only
  signal they can see from outside the tab. The tab title is the conversation's name, not a status
  (`workspace-screen.tsx:333-336`).
- No browser Notification permission is ever requested on the web (`_layout.tsx:808` is Electron-only), and no
  message would trigger one (#11).

### 2.3 iOS app in the background or closed
- Nothing. No push, no local notification, no badge (#13). iOS suspends the app; when it is opened again it reconnects
  and fetches the list and the open timeline fresh (the agent snapshot carries `status`/`activeTurn`,
  `paseo-connection.ts:588-618`).
- `client_heartbeat` reports `deviceType: mobile` and `appVisible`, which the server ignores (#14).

### 2.4 A second tab or device of the same person
- All tabs/devices of a person reach the **same** per-person CloudCLI process (router rendezvous), and its one hub
  (`paseo-hub.ts:1-9`) feeds every connection. A send from device A: the user row and `turn_started` reach every
  connection showing that conversation (`paseo-hub.ts:290-291`); the list rows of all connections switch to running
  (`session_upserted` from `noteSessionActivity`, `chat-run-registry.service.ts:255` → `paseo-hub.ts:319-324`).
- Opened on device B mid-run: the hub reads history, then follows the run from `run.lastSeq`
  (`paseo-hub.ts:211-225`). Code-derived risk: text already streamed but not yet in the transcript file is skipped
  (the subscribe starts at the current seq, so it does not replay those frames).
- **During a ui rollout** the person can have tabs on two processes for up to `DRAIN 900` s. Only session-list frames
  cross over (Postgres NOTIFY, `session-upsert-broadcast.service.ts:67-138`; the hub socket is in `connectedClients`,
  `chat-websocket.service.ts:663`). Code-derived: the old process's hub answers `isRunning` from its **own** registry,
  so a run in the new process shows as `done` in tabs still on the old one, and its timeline rows don't stream there
  until the tab reconnects. The target removes this case: one magpie event log and stateless pigeons.
- `yyc.sandbox_status` goes to every connection (`paseo-connection.ts:813`), but a tab that connects while a sandbox is
  being prepared does not see the chip until the next change: the hub's `preparing` set (`paseo-hub.ts:127`) is never
  sent on connect (code-derived).

### 2.5 Title updates
- New conversation: the provisional name (the first characters, `sessions.service.ts:139-142`) shows at once on every
  tab (`:345-347`). Then the title model runs alongside the first turn, inside the sandbox through egress
  (`jarvis-title.ts:6-13`; `claude-runtime.provider.js:1104-1109`), and is pushed (`jarvis-title.ts:118,145`).
  A rename by the person (`name_source='user'`) always wins (`sessions.db.ts:400-419`).
- Claude Code's own transcript title only replaces a provisional name (`sessions.db.ts:116-119`).
- Live counts (2026-11-02): 15 active conversations carry a generated title, 11 still have the provisional one (1 of
  those created in the last 7 days). Titled ones: 11 created in the last 7 days.
- With no title yet, the bridge shows 「新對話」 (`paseo-connection.ts:555-557`). The browser tab title is the
  conversation's label, or "YYC AI" with none open (`workspace-screen.tsx:2904-2909`).

### 2.6 Background work after the reply (the hold)
- During a hold, the run's status is already `completed` (`chat-run-registry.service.ts:121-128`;
  `claude-runtime.provider.js:1368-1370`), so the loader and favicon show **done** while background tasks are still
  going. Follow-up turns the CLI pushes then append rows with a fresh `turnId` but no `turn_started/turn_completed`
  (`paseo-hub.ts:372-388`), and `task_status`/`task_notification` are dropped (`:339-340`). So staff can see a "done"
  conversation start writing again without a running indicator. Details are in the background-hold inventory area; for
  this gap, magpie must record explicit turn boundaries for CLI-started turns, so #1/#2 become true.

### 2.7 Claude asks a question (AskUserQuestion / ExitPlanMode)
- The runtime sends `permission_request` and waits with **no timeout** (`claude-runtime.provider.js:1145,1181`,
  `:164-170`). The hub drops the frame (`paseo-hub.ts:335-337`), and the bridge says `pendingPermissions: []`
  (`paseo-connection.ts:609`). Staff see a conversation that keeps running with nothing to answer. They can only
  press stop. Meanwhile the sandbox touch goes on every 60 s (`sandbox-process.ts:22,187`), so the sandbox lives until
  the 8 h max (gap-0 row "Touch cadence"). The comment at `claude-runtime.provider.js:1138-1143` claims that bypass
  mode auto-approves these tools. Under Claude Code 2.1.304 it doesn't (inventory "AskUserQuestion / ExitPlanMode in
  YYC": `requiresUserInteraction` is checked before the mode allow).
- The web Notification and favicon "attention" paths for permissions (`session-context.tsx:112-136`,
  `use-favicon-status.ts:33`) would light up if the bridge sent pending permissions. That's the one place where the
  dead attention code would earn its keep.

---

## 3. Dead code and data to remove in the cut

Server (goes away with CloudCLI anyway; listed so nothing is ported by mistake):
- `server/modules/notifications/**` (orchestrator, VAPID, desktop WS, `/api/notifications/endpoints`
  `notifications.routes.ts:42,60,87,110`), `server/index.ts:339-340`, the `/desktop-notifications` branch
  `websocket-server.service.ts:116-119`, settings routes `settings.routes.ts:41-49`, and the call sites in
  `claude-runtime.provider.js` listed in #16.
- Scheduled messages: `server/index.ts:180,376-378` (0 rows live).
- Tables per `cc_<userKey>` schema (live, all 10 schemas): `push_subscriptions` 0 rows, `notification_channel_endpoints`
  0 rows, `user_notification_preferences` 1 row each, all the default (`webPush/desktop/inApp: false`), `vapid_keys`
  1 key pair each (an unused private key). The import into magpie carries **none** of these: nothing to migrate.

App (Paseo stays; this code is inert and can stay or be trimmed later, but pigeon must not "implement" it by accident):
- The heartbeat (#14), push token (#15), attention clear/mark unread (#9), `agent_attention_required` and
  `terminal_attention_required` handlers (#11, #12). pigeon keeps answering correlated requests it doesn't support
  with `rpc_error code: unsupported` (today `paseo-connection.ts:501-510`), and keeps ignoring
  `client_heartbeat`/`abort_request`/`audio_played` (`:201-204`). The app already swallows these errors
  (`use-agent-attention-clear.ts:62`).
- Router keep-alive by socket traffic (`router.mjs:1200-1205`) goes with the router. Nothing in the target needs
  presence: sandboxes are kept alive by turns, not viewers (`sandbox-process.ts:187` touches only while Claude Code
  runs; `jarvis3 web/app.ts:89-93`: a warm does not reset a live sandbox's idle time).

## 4. Requirements on the new services (to keep what is live)

- **magpie**: a conversation's state is part of what it serves. That covers `running` (with `turnId`, `startedAt`),
  the last turn's outcome (completed/failed/canceled), `preparing`, the title with `name_source`, and `updated_at`.
  All of it changes through events, so any pigeon replica can (a) render the list snapshot (`fetch_agents` /
  `fetch_workspaces`) with correct status for runs started anywhere, and (b) tail list-level changes. CLI-started
  follow-up turns get explicit turn-start/turn-end events (2.6). Title generation (provisional at create, generated
  after the first message, never over a user rename) is magpie's; the model call goes to `model` through the sandbox
  gateway as today, so its cost lands under the conversation.
- **pigeon**: maps magpie's state to the Paseo fields the app reads: workspace `status running|done`, agent
  `status running|idle`, `activeTurn`, `turn_*` stream events, `workspace_update`/`agent_update` upserts and removals,
  `yyc.sandbox_status` (sent on connect too, fixing 2.4), and `requiresAttention:false` (or real values if section 5
  adds them). It also forwards `POST /api/jarvis/warm` to hermit `warm` (fixed endpoint name) with the same 30 s
  per-conversation coalescing (`jarvis-warm.routes.ts:15,47-58`), and nothing for read-only view-as
  (`warm.ts:13-14`).
- **hermit** (YYC): `warm` keeps today's meaning: a demand hint for the scaler, counted for 120 s, that doesn't create
  a sandbox or reset a live one's idle time (`jarvis3 web/app.ts:88-106`). It also tells magpie/supervisor when a
  sandbox is preparing or ready, which drives the chip.
- **supervisor**: reports turn start/end for every turn (including CLI-started ones) and background-task state to
  magpie, and holds a pending AskUserQuestion/ExitPlanMode as a stored event instead of an in-memory promise
  (section 5).

## 5. Decisions this gap surfaces (not made here)

1. **AskUserQuestion / ExitPlanMode.** Either (a) show them: the supervisor holds the `can_use_tool` request, magpie
   stores it as a pending ask event, pigeon sends `agent_permission_request` and `pendingPermissions`, and the app's
   existing UI (`session-context.tsx:490-554` per the inventory) answers it. That also lights up the favicon
   "attention" state for free (#3). Or (b) disallow both tools (`--disallowedTools`) for a read-only analysis
   product. Today's behaviour (an endless spinner) should not be carried over either way.
2. **Notifications (none today).** Leaving them out matches today and the agreed design (which names none). If staff
   want "reply finished" notices later, that's a new feature, not a port. Web Push needs one instance-wide VAPID key
   (a Sealed Secret from `yyc`), subscriptions kept by magpie (pigeon holds no state), and a sender triggered by
   magpie's turn-end events. iOS would need an APNs key, the Push capability on `dev.de0ch.yycai` and removing
   `with-no-push-entitlement.js`. Unread/attention dots would need a per-person "seen up to seq" in magpie. Nothing in
   CloudCLI's notification module is worth porting for either.

# Gap 0: every numeric limit and quota in today's YYC AI, and what staff see when one is hit

Research only (2026-11-02). Sources: the repos at their checked-out heads (claudecodeui fork, jarvis3, YYC), the
inventory (`inventory.json`, cross-checked, not copied blind), and read-only checks on YYC's cluster:
- the live Deployments in `jarvis` match the manifests (ui 2 x 1536Mi / cpu 2, web 256Mi, egress 256Mi, tools 128Mi,
  cloudflared 64Mi; ui env MAX_USERS 4, CHILD_HEAP_MB 512, IDLE 30, DRAIN 900);
- live budget spend: USD 25.66 over 1,802 model calls since `budget.since` (limit USD 100);
- live `vm_hosts`: the gateway `yyc-sbx-gw` (mem_reserve = mem_total, so 0 slots: it runs no sandboxes) and one AWS
  host `aws-ba2fae` (16.46 GB, 4 CPUs, reserve 1 GiB).

"Staff see" is what the YYC web/iOS app shows. "Agent sees" is what Claude Code inside the sandbox gets (a tool
error, an API error). Where the outcome is worked out from code and not watched live, it says **code-derived**.

Target home column: where the limit lives after the refactor (pigeon / magpie / hermit / supervisor / egress /
tools / app / gone). "gone" = a limit that only exists because of a component being removed (the per-person
CloudCLI process, the router).

---

## 1. Per-sandbox resources (hermit's YYC implementation)

| Limit | Value | Where | When hit: agent / staff | Target |
|---|---|---|---|---|
| Sandbox memory | 1 GiB (`sandbox.memory` "1Gi"; code default the same) | YYC `yyc/deploy/config.json:21`; `jarvis3 chat/src/common/config.ts:226`; applied as the runsc cgroup `memory.limit` at `chat/sandboxd/daemon/src/runsc.rs:163` | gVisor has no OOM killer of its own: when sentry + gofer + the sandbox's memory reach the limit, the host memcg OOM killer kills the **sentry, so the whole sandbox dies** (`docs/SANDBOX-VM.md:528-532`). Usable is about the limit minus 50-70 MiB. Agent: nothing (it is dead). Staff (**code-derived**): the gVisor netstack disappears without a FIN, so the UI's `/proc` socket stays open and the turn keeps spinning; Stop only takes effect after TCP retransmission gives up (about 15 min); a new send gets RUN_IN_PROGRESS. Same outcome as the inventory's host-loss and max-lifetime rows. The next turn's `ensure()` finds the agent unhealthy, releases it (`release_reason 'unhealthy'`, `chat/src/web/sandbox.ts:131-142`) and makes a fresh sandbox; installs and /tmp are gone, /workspace survives. | hermit (value), supervisor (must be OOM-protected itself and report the death so magpie can end the turn with an error instead of a spinner) |
| Sandbox CPU | 1 core (`sandbox.cpu` "1"), CFS quota 100,000 us per 100,000 us period | `config.json:20`; `runsc.rs:164` | Throttled, never killed: commands run slower. Nothing shown. | hermit |
| PIDs | 4,096 per sandbox | `runsc.rs:165` | fork/clone fails with EAGAIN inside the sandbox ("Resource temporarily unavailable" in a Bash tool result). Staff see nothing beyond the tool output. | hermit |
| Open files | RLIMIT_NOFILE 65,536 soft and hard | `runsc.rs:114` | EMFILE "Too many open files" in a tool result. | hermit |
| /dev and /dev/shm | tmpfs 64 MiB each (`size=65536k`) | `runsc.rs:131,133` | ENOSPC when writing /dev/shm (e.g. Chromium, multiprocessing shared memory) as a tool error. | hermit |
| Scratch disk (root overlay upper layer, /tmp, /var/lib/docker) | 5 GiB sparse ext4 per sandbox, 0 reserved blocks | `chat/sandboxd/daemon/src/storage.rs:2-4,19,105-107`; mounts `runsc.rs:138-142` | ENOSPC ("No space left on device") as a tool error; the turn goes on. Not persisted: gone at release. The managed CLAUDE.md tells the model "/tmp (up to 5 GiB, not kept after a release)" (`chat/src/web/claude-code.ts:26`). | hermit |
| Env passed to a sandbox | at most 128 entries and 16 KiB | `sandboxd/daemon/src/config.rs:273-274` | sandboxd answers 400 to the create, web's start fails, staff see `sandboxFailed` (未能啟動這個對話的電腦…). Today's env is far below it. | hermit |
| Spec bounds | cpu in (0, 64], memory >= 128 MiB | `config.rs:454-458` | 400 on create (config error only). | hermit |
| Image unpack / pull | pulls serialised per digest, 4-slot table, waits up to 20 min; image pull call timeout 15 min | inventory (images.rs); `chat/src/web/sandbox-vm.ts:290,597` | A start that needs a pull is slow (the `preparing` chip); a failure ends as `sandboxFailed`. | hermit |

## 2. Persistent storage (JuiceFS) quotas

| Limit | Value | Where | When hit | Target |
|---|---|---|---|---|
| /workspace capacity | 1 GiB per conversation (`juicefs quota set --capacity 1`) | `storage.rs:20,291-345` (WORKSPACE_GIB, set_quota, 60 s timeout on the command at :340); docs `jarvis3/docs/CLAUDE-CODE.md:27-28,50-58` | Agent: EDQUOT "Disk quota exceeded" (errno 122) on write, or on `close`/`fsync` for the write that crosses it (the gVisor fork's close-flush patch, `sandboxd/gvisor-patches/0001-gofer-close-flush-error.patch`). Usage is refreshed every few seconds, so a burst overshoots a little. The turn continues; the model tells the person. The managed CLAUDE.md states the limit (`claude-code.ts:20-26`), including a now-obsolete "check big writes" rule (`claude-code.ts:9-11,24-26`). Staff see only what the model says. | hermit (it provides /workspace) |
| /workspace files | 1,000 inodes per conversation (`--inodes 1000`) | `storage.rs:21`; measured 1,007 before the cap held (`CLAUDE-CODE.md:31-32`) | Same EDQUOT path. Typical trigger: `node_modules`, unpacked archives, virtualenvs in /workspace. | hermit |
| Quota only set on a NEW folder | set only when `ws/<id>` did not exist at the first sandbox create | `storage.rs:292-301` | Gap: conversations whose first message had an attachment (the UI makes `ws/<id>/uploads` first, `claudecodeui server/modules/jarvis/jarvis-conversations.ts:109-136`), forks (`jarvis-conversations.ts:86`) and "continue a share" (`jarvis-shares.ts:462`) have **no quota at all** (live: 3 of 37). A failed `quota set` returns 500 to the create (staff: `sandboxFailed`), and the retry then skips the quota because the folder now exists. | hermit (set the quota whenever the workspace is provided, idempotently) |
| ~/.claude (users/<key>/claude), shares/, sent/ | **no quota** | `storage.rs:152-156` (mkdir + chown only); live `juicefs quota list` shows only `/ws/*` | Grows without bound (largest user folder 127 MiB). The supervisor/magpie design removes ~/.claude from JuiceFS, so this becomes magpie's storage. | magpie (per-conversation event/blob storage needs its own cap, or an explicit "none") |
| Share / fork copies | full `cp` of the workspace, up to 1 GiB each | `jarvis-conversations.ts:75-90`, `jarvis-shares.ts:244-258,447-462` | No limit on how many; each duplicates bytes. | magpie (shares) + hermit (workspace clone) |
| JuiceFS cache, sandbox hosts | 4 GiB (`--cache-size 4096`), no writeback, open-cache 0 | `chat/sandboxd/host/setup.sh:48-88` | Not user-visible. | hermit |
| JuiceFS cache, UI mount pods | 1 GiB cache, 64 MiB buffer (`cache-size=1024, buffer-size=64`) | YYC `yyc/deploy/juicefs.yaml:12,78` | Not user-visible. Goes away with the UI's /jfs mount. | gone (pigeon/magpie don't mount JuiceFS) |
| JuiceFS mount pod memory | request = limit 192Mi (header comment says 320 Mi: stale) | `juicefs.yaml:7-8` vs `:47-50` | An OOM-killed mount pod cuts the UI's /jfs (inventory "JuiceFS mount-pod restart"). | gone |
| JuiceFS CSI node plugin memory | 384Mi (128 Mi was OOM-killed) | `juicefs.yaml:55-58` | Same. | gone |
| JuiceFS trash | TrashDays 30 (Deyao, 2026-11-16) | live `jfs_setting` (`juicefs status`) | A deleted file stays in the volume's trash for 30 days. | hermit |
| PV size | 1Pi (nominal, a static PV) | `juicefs.yaml:74,97` | Not a real limit. | gone |
| Sent-file snapshot request | at most 100 files, body <= 256 KiB, `juicefs clone` 30 s per file | `claudecodeui/jarvis/router.mjs:737,744` (`readLimited(req, 256 * 1024)`, `files.length > 100`), `:788` | Over 100 files: 400 "toolUseId and files (paths) are required" -> the hook blocks the tool: "The files were not sent: they could not be saved for the user (HTTP 400: …). Try sending them again." (`chat/sandbox/send-file-hook.mjs:42-45,55`), exit 2, which Claude Code shows the model as the SendUserFile tool's error. Staff see the model's explanation, no files. | magpie (sent-file snapshots) |
| SendUserFile hook | hook timeout 60 s (Claude Code setting); the POST times out at 45 s | `chat/src/web/claude-code.ts:78`; `send-file-hook.mjs:29,36` | "no answer within 45 s" -> same block message. | supervisor/magpie |

## 3. Pool, placement and scaling (hermit)

| Limit | Value | Where | When hit | Target |
|---|---|---|---|---|
| Live sandboxes per instance | `maxActive` 10 (code default 6) | `config.json:19`; `config.ts:226`; checked under advisory lock `CAPACITY_LOCK` at `sandbox.ts:172-199` | The least recently touched sandbox idle >= 2 min is released ('pool', `sandbox.ts:178-183`); its conversation silently gets a fresh sandbox next turn. With none idle, the request polls every 2 s for up to `POOL_WAIT_MS` 60 s (`sandbox.ts:67,151-160`), then throws `PoolFullError("the sandbox pool is full, try again in a minute")`. **That is a plain Error to the HTTP layer** (`chat/src/web/app.ts:47-50`: anything not HttpError is 500 "internal error"), so the fork's mapping (`claudecodeui server/modules/jarvis/jarvis-errors.ts:64-71`: 429/503 -> `busy`) never fires: staff see `sandboxFailed` 「未能啟動這個對話的電腦，請再試一次；如問題持續，請通知管理員。」 instead of `busy` 「所有電腦正在使用中，請過幾分鐘再試。」. During the wait they see the `preparing` chip (after 1 s, `sandbox-process.ts:85`) and after 20 s "This can take a minute or two" (`paseo/packages/app/src/yyc/sandbox-chip.tsx:12,16`). | hermit (and the "busy" answer must be a distinct status the app shows) |
| Slots per host | min((mem_total − reserve) / sandbox.memory, cpus × cpuOvercommit / sandbox.cpu, max_sandboxes) | `chat/src/web/sandbox-vm.ts:186-192`; `cpuOvercommit` 3 (`config.ts:271`) | Live: the gateway gives 0 (it runs no sandboxes); an m7i.xlarge gives min(14, 12) = **12** (CPU-bound; `docs/SANDBOX-VM.md:151-152` says 15: stale). | hermit |
| Host memory backstop | no placement on a host with MemAvailable < 256 MiB (`minAvailableMemory`) | `config.ts:272`; `sandbox-vm.ts:224-228` | Placement skips the host; may trigger a launch. | hermit |
| AWS hosts | `maxHosts` 3 (+1 only while one is retiring); YYC's eu-central-1 quota is 16 vCPU and stays | `config.json:26`; `sandbox-vm.ts:325-333`; `config.ts:109`; `SANDBOX-VM.md:39,333` | `launch()` returns "full": `create` throws `PoolFullError("all sandbox hosts are full, try again in a few minutes")` (`sandbox-vm.ts:758`) -> same 500 -> `sandboxFailed`. In practice maxActive 10 < 12 slots of one host, so the pool cap bites first. | hermit |
| Placement wait | `PLACE_WAIT_MS` 170 s, retry every 3 s | `sandbox-vm.ts:41,751-761` | "no sandbox host became available in time" -> 500 -> `sandboxFailed`. | hermit |
| Spare slots | `spareSlots` 0 | `config.json:25`; `config.ts:82,270` (default 1) | With no AWS host up, the first conversation waits for an EC2 boot (about 1 min+): the preparing chip. Typing warms a host (below). | hermit |
| Demand hint window | 120 s per person (`DEMAND_WINDOW_S`); the app sends `warm` at most once per conversation per 30 s while typing | `sandbox.ts:50`; `claudecodeui server/modules/jarvis/jarvis-warm.routes.ts:15`; app `yyc/warm.ts` | Not visible. | pigeon -> hermit (warm) |
| Host lifetime | an empty AWS host is terminated once >= 900 s old (`minHostLifetimeSeconds`); `emptyMinutes` 5 is unused | `config.json:25-26`; `config.ts:90-91,113-114`; `sandbox-vm.ts:706-709` | Not visible (cost). | hermit |
| Host boot / health | boot timeout 8 min; host dead after 30 s silent; hub dead after 120 s; orphan EC2 instance 10 min | `sandbox-vm.ts:36-40` | A host judged lost: its sandboxes are gone (see the memory row for what staff see). | hermit |
| Sandbox start | `START_TIMEOUT_MS` 180 s for the whole ensure loop, and again 180 s for `waitReady` (worst case ~360 s); a `starting` row older than 5 min is cleared by the janitor | `sandbox.ts:68,125-163,214`; `janitor.ts:24-30` | "the sandbox did not start in time" / "the sandbox's agent did not answer in time" -> 500 -> `sandboxFailed`. | hermit |
| Web HTTP | `requestTimeout` 300 s, keep-alive 65 s; JSON bodies <= 1 MiB (413 "request too large") | `chat/src/web/main.ts:32-34`; `http.ts:21-26` | A start longer than 300 s is cut. The fork's fetch to web: `AbortSignal.timeout(20 min)` (`sandbox-process.ts:46`), but Node's fetch has a 300 s headers timeout, so a > 300 s ensure ends as `unreachable` 「暫時連接不到這個對話的電腦，請稍後再試。」 while web goes on building an unused sandbox (inventory). | hermit (REST, with a status the caller can map) |
| sandboxd API | 16 connections, 24 sandboxes per host (`maxSandboxes`, max 4096), 4 migrations at once, 64 KiB max body (16 KiB-16 MiB allowed), 2 image connections, helpers 384 MiB, idle 6 s, request read 30 s | `sandboxd/daemon/src/config.rs:40-53,252-266`; `server.rs:244,246`; errors `sandboxes.rs:444` ("this host has limits.maxSandboxes"), `:618` ("too many migrations in flight") | 503 to the controller; a create failing this way -> `sandboxFailed`. | hermit |
| sandboxd memory | fixed region, RSS <= 31.5 MiB, cgroup <= 44 MiB; unit MemoryMin 160M / High 384M / Max 512M; /sbx capped at MemTotal − reserveMemory (1,536 MiB default, 1 GiB on AWS hosts) | `budget.rs:9-33`; `main.rs:86-92`; `host/setup.sh:125-165`; `sandbox-vm.ts:377` | Full tables -> 503, oversize -> 413; never user-facing directly. | hermit |
| Live migration | pre-copy target downtime 100 ms, max 20 rounds, cross-cloud abort above 500 ms, only sandboxes using <= 0.05 cores move across clouds; receive/send calls 15 min; relay 900 s; 3 rsync rounds of scratch (stop early under 32 MiB / 2,000 files) | `config.ts:99,277`; `sandbox-vm.ts:466-485`; `sandboxes.rs:27-29`; relay.rs (inventory) | Staff see a ~0.1 s stutter, or nothing. Known limit: a sandbox whose memory plus overlay writes approach 1 GiB is OOM-killed on restore (`SANDBOX-VM.md:627-640`) -> the memory row's outcome. | hermit |
| Host port range | 100 ports per host for sandbox agents (7100-7199), restore offset 1000 | `sandboxes.rs:22-25` | Above the 24-sandbox cap anyway. | hermit |
| Gateway peers / egress targets | 32 peers, 64 egress targets | `sandboxd/daemon/src/net.rs:213-214` | More AWS hosts than 32 can't be routed (far above maxHosts). | hermit |

## 4. Lifetimes and idle times

| Limit | Value | Where | When hit | Target |
|---|---|---|---|---|
| Sandbox idle release | 30 min without a touch (`idleMinutes`) | `config.json:17`; `janitor.ts:15-21` (every 30 s, `startJanitor` :56) | Installs, processes, /tmp gone; next turn gets a fresh sandbox silently (the managed CLAUDE.md explains it, `claude-code.ts:17-19`). Staff see the preparing chip on the next send. | hermit |
| Touch cadence | the fork touches every 60 s while Claude Code runs (`TOUCH_EVERY_MS`); a sandbox is a pool victim only if untouched >= 2 min | `sandbox-process.ts:22,186-188`; `sandbox.ts:180` | A hung AskUserQuestion keeps a sandbox alive indefinitely (up to 8 h). | supervisor/magpie (touch is the turn being alive) |
| Sandbox max lifetime | 8 h from creation (`maxLifetimeHours`), `expires_at` never extended | `config.json:18`; `sandbox.ts:190-192`; `janitor.ts:17-18` | Released **even mid-turn** (runsc kill, no FIN). Staff (**code-derived**): endless spinner, Stop takes ~15 min, new sends RUN_IN_PROGRESS; if a reset arrives: `sandboxFailed`. Live: none hit yet. | hermit (should defer while a turn runs, or the supervisor must end the turn cleanly) |
| Provider keep-alive grace | provider timeout = idle release + 5 min, keep-alive at most every 30 s | `sandbox.ts:69-71,90-101` | e2b only; no effect on vm. | hermit |
| Per-person CloudCLI process idle | stopped after 30 min with no traffic and 0 running chats, or 60 min if the count is unknown; sweep every 60 s | `router.mjs:603,608,932-942`; `ui.yaml:81` | Next request starts a fresh child (start budget 60 s, `router.mjs:605`): a slower first load. A wedged child mid-turn can be stopped (inventory). | gone |
| Per-pod user processes | `JARVIS_UI_MAX_USERS` 4 per pod (code default 6), 2 pods | `ui.yaml:79`; `router.mjs:73,602,897-899,907-914` | At the cap with every process running a chat: a new person gets 503, HTML page 「暫時未能使用 / 系統繁忙，請稍後再試。The service is busy, try again shortly.」 (`router.mjs:988`) or "busy" on the WebSocket upgrade (`router.mjs:1188`). So **at most 8 people at once** across the 2 pods, fewer if the rendezvous hash is uneven. | gone (pigeon has no per-person processes) |
| CloudCLI child heap | `--max-old-space-size` 512 MB (code default 256) | `ui.yaml:80`; `router.mjs:77,496,606` | V8 aborts (SIGABRT): the app's socket closes and reconnects; the running turn just stops with no error; a message sent during the sandbox wait can be lost (inventory "child crash"). An 86 MB transcript of base64 page renders crashed a 192 MB child in a loop (why it is 512). | gone (magpie pages events, so no whole-transcript heap) |
| Child start | must answer /health within 60 s; SIGKILL 10 s after SIGTERM | `router.mjs:605,607` | "start timeout": the request fails (503 page). | gone |
| Background-work hold | 30 min of silence (`BG_WAIT_CEILING_MS`, also passed to the CLI as `CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`) | `claudecodeui server/modules/providers/list/claude/claude-runtime.provider.js:73-91,245,1035`; forwarded by `sandbox-process.ts:25` | Background work that never reports is abandoned after 30 min (plus up to 30 min of CLI wind-down). Nothing shown. | supervisor |
| Tool approval wait | 55 s (`CLAUDE_TOOL_APPROVAL_TIMEOUT_MS`) | `claude-runtime.provider.js:72,142` | Under bypassPermissions, AskUserQuestion/ExitPlanMode can hang (inventory "AskUserQuestion / ExitPlanMode in YYC"). | supervisor |
| Run-registry retention | 5 min after a run (re-armed while a hold has tracked tasks) | `claudecodeui server/modules/websocket/services/chat-run-registry.service.ts:39-91` (inventory) | A late subscriber to a held run with no tracked tasks is not attached. | gone (magpie events by seq) |
| Recently deleted | purged for good 30 days after deletion (`RECENTLY_DELETED_DAYS`); the process purges 30 s after start then hourly; the router wakes explicit users 10 min after start then every 24 h | `server/modules/providers/services/sessions.service.ts:110,759,771`; `jarvis-purge.ts:10-11`; `router.mjs:129-155,611-612` | The app's Recently deleted shows `purgeAt`. Gap: `@yyc.hk` staff and removed users are never woken, so their expired deletions linger until they sign in (inventory). | magpie |
| View-as token | 12 h (`JARVIS_VIEW_AS_HOURS`) | `router.mjs:627` | The admin's view ends; 403. | pigeon |
| Access session | 720 h (30 days) for the staff app; the iOS token lasts the same | YYC `news-stream/deploy/cloudflare/tofu/access.tf:69`; app `yyc/access-token.ts` (inventory) | Sign-in again (signed-out shell). | pigeon (sign-in) |
| Access identity cache | 6 h per email (5 min after a failed lookup); JWKS refetched hourly, at most every 30 s | `router.mjs:240-280` | Not visible. | pigeon |
| ops_events sandbox_start | kept 30 days | `janitor.ts:46-47` | Operator page history only. | hermit |

## 5. Budget, spend and model-provider limits (egress)

| Limit | Value | Where | When hit | Target |
|---|---|---|---|---|
| Shared budget | USD 100 for everyone together since `budget.since` 2026-10-21T02:05:52Z; no per-person budget, no automatic reset | `config.json:67`; `chat/src/common/config.ts:120-130,305-312`; `common/budget.ts:1-37` | Every model call answers 402 `{type:"error",error:{type:"budget_exceeded",message}}` (`egress/proxy.ts:582-594`) with 「Deyao 設定了 100 美元的使用上限，目前已用完。」 (`budget.ts:64-68`). Claude Code writes `API Error: 402 {...}` into the transcript and the fork shows the message as-is (`jarvis-errors.ts:106-108`). The turn ends. Titles also fail (same route). Calls in flight finish, so spend overshoots by about one call per running conversation. Live: USD 25.66 used. | egress (cost, budget). The supervisor/magpie path must still surface the 402 text. |
| Budget re-read | at most every 0.5 s per egress pod (`BudgetGate(db, budget, 500)`; the class default is 2 s) | `egress/main.ts:43-44`; `budget.ts:45` | Overshoot window. Budget sums `egress_log` from `since` with no retention, so pruning `egress_log` would lower the spend (inventory). | egress |
| OpenRouter key cap | USD 50 per calendar month on the key `yyc-chat` (backstop above the app budget) | YYC `yyc/infra/openrouter.py:25`; `yyc-ai/README.md:239-240` | Reached before the app budget only if more than USD 50 is spent in one month (the app budget is USD 100 cumulative, so this can bite first). OpenRouter then refuses the key; the fork maps 402 to `budget` 「此服務的使用預算已用完，請聯絡管理員。」 or 401/403 to `modelAccess` 「AI 模型服務拒絕存取，請通知管理員。」 (`jarvis-errors.ts:108,111`), whichever status OpenRouter returns. | egress |
| Model rate limits | none of our own; OpenRouter/upstream 429s pass through | `jarvis-errors.ts:109` | 「目前向 AI 模型發出的請求太多，請稍等一會再試。」 after Claude Code's own retries. | egress |
| Model request body | 64 MiB per request; the bodies egress holds at once share one 64 MiB FIFO pool per pod (a chunked body counts as 64 MiB) | `egress/proxy.ts:488-509,623-625`; `modelbody.ts:84-123` (inventory) | 413 `request_too_large` "egress: a model request body is limited to 67108864 bytes" -> fork: 「AI 模型拒絕了這個請求: egress: a model request body is limited to 67108864 bytes」 (`jarvis-errors.ts:112`). **Code-derived consequence:** every earlier image is re-sent with each turn (`jarvis-images.ts:22-24`), each up to 3.75 MB raw / 5 MB base64, so a conversation with about a dozen large images (plus screenshots read by tools) can no longer make any model call; every later turn fails the same way. Queueing on the pool is invisible (a slower start of the reply). | egress (cap), magpie/supervisor (should keep images by reference so the body stays small) |
| Model cost sniffing | JSON responses parsed up to 20 MiB for usage | `proxy.ts:682` | A cut SSE stream has no usage event, so its cost is missing from the budget (OpenRouter still bills it; inventory). | egress |
| Context window | `contextWindow` 1,048,576 for DeepSeek V4.1 Flash, passed as `CLAUDE_CODE_MAX_CONTEXT_TOKENS` (else Claude Code assumes 200k and auto-compacts near 170k); the fork's own token-usage display falls back to 160,000 (`CONTEXT_WINDOW`) | `config.json:8`; `chat/src/web/claude-code.ts:45-53`; `claude-runtime.provider.js:491-496` | Claude Code auto-compacts near the window; over it the provider refuses (model-refused text). | supervisor (env for Claude Code) / egress (model list) |
| Model allowlist | only `models[]` ids and `titleModel` | `config.ts:220-223`; `proxy.ts:582-590` | 400 "egress: model X is not offered here" -> `modelRefused` text. | egress |
| Title call | first message cut to 2,000 chars; 45 s fetch timeout; title via /proc 60 s and 1 MB stdout | `claudecodeui server/modules/jarvis/jarvis-title.ts:21,45,72,115`; `sandbox-process.ts:286,315-320,337` | The conversation keeps its fallback title. | magpie |

## 6. Egress network limits (the sandbox's only path)

| Limit | Value | Where | When hit | Target |
|---|---|---|---|---|
| Upload per connection | 8 MiB client->upstream per CONNECT tunnel or forward request (`maxUploadBytes`; not in YYC's config, so the default) | `config.ts:185-186,227`; `egress/proxy.ts:27-28,88,267-276,358-367` | The connection is destroyed mid-upload; logged `deny:upload`. Agent: a network error in the tool result (e.g. curl "Connection reset by peer", "Empty reply"). Not applied to `/x` routes or the mirror. Staff see only the model's account. | egress |
| Ports | 80 and 443 only | `proxy.ts:90,205-209` | 403 "egress: port N is not allowed (only 80, 443)." in the tool output. | egress |
| Allowlist / threat DNS | generated allowlist; threat DNS 2 s timeout, 2 tries, fail closed; answers cached 30 s-5 min, cache cleared above 10,000 names | `egress/threatdns.ts:4,25-41` | 403 "egress: <why>; the sandbox cannot reach it." The managed CLAUDE.md says a refused site cannot be enabled (`claude-code.ts:29-30`). | egress |
| Upstream connect | 10 s TCP connect timeout | `proxy.ts:740-745` | 502 "egress: could not connect to host:port". | egress |
| Wire (hosted sandboxes) | WebSocket frames <= 4 MiB, ping every 30 s; the sandbox side pauses above 4 MiB buffered | `proxy.ts:128,152`; `chat/sandbox/agent.mjs:50` | e2b provider only (unused in YYC). | egress |
| Package cache | 5 GiB LRU on disk (`cacheGiB`), entries above a quarter of it not cached; index files 5 min | `config.json:49`; `egress/cache.ts:100,136-140`; `proxy.ts:57` | Not visible (slower installs). | egress |
| Egress token cache | 5 s, map cleared above 5,000 | `egress/identity.ts:32,52` | A released sandbox's token works up to 5 s more. | egress |
| Egress log batch | flush every 1 s or 200 rows; path cut at 2,000 chars | `egress/egresslog.ts:37-38,49` | Not visible. | egress |
| Egress pod | 256Mi request = limit, cpu limit 1, ephemeral 1-7Gi; 2 replicas | `jarvis3/deploy/chat/egress.yaml:66-67`; YYC `kustomization.yaml:46-49` (128 Mi got both pods OOM-killed on a 7 MB conversation, 2026-10-28) | An OOM kill cuts every sandbox's model stream and tunnels on that pod: Claude Code retries; if its retries run out, 「連接不到 AI 模型，請再試一次。」 (`jarvis-errors.ts:113-114`). | egress |

## 7. Tools (MCP) limits

| Limit | Value | Where | When hit | Target |
|---|---|---|---|---|
| MCP request body | 2,000,000 bytes | `chat/src/tools/server.ts:12,68-80` | The request fails ("too large") and Claude Code shows the tool call as failed. | tools |
| tools HTTP server | requestTimeout 180 s, headers 60 s, keep-alive 65 s | `server.ts:117-119` | A call beyond 180 s is cut. | tools |
| Upstream MCP (yyc-mcp) | 120 s per call; tool list refreshed every 10 min, at most 20 pages; exposed names cut to 64 chars | `chat/src/tools/upstream.ts:22,80,114,153` | "yyc answered HTTP …" / timeout as a tool error the model sees. | tools |
| yyc-mcp rate | 120 calls per minute per identity; **all of YYC AI shares one identity** (`yyc-ai@ai.yyc-internal.com`), so one bucket for every staff member | YYC `news-stream/mcp-edge/src/registry.ts:37-47`; `news-stream/deploy/apps/yyc-mcp.yaml:40,120` | HTTP 429 -> the tool call fails with "yyc answered HTTP 429: …"; the model usually retries or reports it. | tools (YYC-specific: stays in tools) |
| web_search | num_results 1-25 (default 8) | `chat/src/tools/builtin.ts:33,51` | Clamped silently. | tools |
| web_fetch | max_chars 1,000-200,000 (default 20,000), cut text says "[… cut at N characters of M; ask with a larger max_chars for more]"; direct fetch at most 8,000,000 bytes, 5 redirects, 30 s overall; capture service 90 s | `builtin.ts:87,124,133,151-152,173`; `tools/netguard.ts:64-79` | The model sees the cut marker and can ask for more; a binary URL gets "The URL is a … file … Download it in the sandbox with bash". **Code-derived interaction:** a 200,000-char result is above Claude Code's default MCP output cap (`MAX_MCP_OUTPUT_TOKENS`, documented default 25,000 tokens; not overridden in the sandbox image `chat/sandbox/Dockerfile:101-109` or `claude-code.ts:46-64`), so Claude Code truncates/refuses it before the model sees it. | tools / supervisor (Claude Code env) |
| Exa calls | 80 s default timeout; highlights 400 chars; snippet 600 chars | `tools/providers.ts:6-19,68,85` | "Exa did not answer within 80 s" as a tool error. | tools |
| TikHub | 45 s; limit 1-30 (default 15); per-call cost USD 0.001-0.01 by platform | `providers.ts:125-132,350`; `builtin.ts:208,219` | Tool error text. Costs are logged, not budgeted (the budget counts model cost only, `budget.ts:1-3`). | tools |
| AnyAPI | search/describe/result 30 s, run 120 s; limit 1-30 (default 10); max_items 1-500; result text cut at 40,000 chars; result re-readable free for about 15 min | `providers.ts:373-466`; `builtin.ts:236-284,303-310,378-389` | Balance out: "AnyAPI's balance has run out, so its APIs can't be used until it is topped up (the admin has to add funds)." (`providers.ts:393-394`) as the tool error; the model relays it. | tools |
| tools pod | 128Mi (YYC patch), cpu 1, 2 replicas; drain 150 s, grace 180 s | YYC `kustomization.yaml:61-64`; `jarvis3/deploy/chat/tools.yaml:25,43,56-57` | OOM: in-flight tool calls fail as tool errors. | tools |

## 8. Front door, sign-in and the per-person UI path (pigeon replaces)

| Limit | Value | Where | When hit | Target |
|---|---|---|---|---|
| Cloudflare request body | Cloudflare's proxy limit (100 MB on Free/Pro; the zone's plan is not recorded in the tofu) | Cloudflare platform; tunnel `news-stream/deploy/cloudflare/tofu/tunnels.tf:61-74` | An upload above it gets Cloudflare's 413 page; the app shows "Upload failed." | pigeon (deployment fact, not config) |
| Cloudflare origin response | 100 s to the first byte of an HTTP response (Cloudflare's 524) | Cloudflare platform | Matters for any long non-streamed REST call through the front door (e.g. a blocking turn start). Streams and WebSockets are not affected. | pigeon |
| Router HTTP | keep-alive 65 s, headers 70 s, no request timeout (streams) | `router.mjs:647-649` | — | pigeon |
| Router drain | 900 s for running chats on SIGTERM, grace 960 s, preStop 10 s | `router.mjs:604`; `ui.yaml:44,82,103-104` | A turn running longer than 900 s during a rollout is cut (inventory). | pigeon (stateless: nothing to drain but sockets) / supervisor (turns no longer tied to a UI socket) |
| ui pod | 1536Mi request = limit, cpu 2, ephemeral 256Mi-2Gi | `ui.yaml:107-108` | 4 x 512 MB heaps plus RSS only barely fit; a cgroup OOM may kill the router and every person on the pod loses the turn. | gone |
| Express JSON / urlencoded bodies | 50 MB on every route | `claudecodeui server/index.ts:97-108` | 413 from Express. | gone |
| Peer lookups | 2 s timeout, 1 s connect; peers refreshed every 5 s | `router.mjs:670,797-815` | Split-brain risk (inventory). | gone |

## 9. Message composition, uploads and history (app + CloudCLI server -> pigeon/magpie)

| Limit | Value | Where | When hit | Target |
|---|---|---|---|---|
| File attachment, app check | 50 MB per file (`FILE_UPLOAD_MAX_BYTES`), one file per POST | `paseo/packages/app/src/attachments/upload-limit.ts:2`; `composer/actions.ts:158-185` | Toast 「<file> 太大（上限 50MB）」 (YYC resources `i18n/resources/yyc.ts:121`; note `zh-HK.ts:189` still has the English text "is too large (max 50MB)"). | app (must match pigeon/magpie's cap) |
| File attachment, server | **30 MB per file**, 10 files per request (multer `attachmentUpload`) | `claudecodeui server/modules/assets/assets.routes.ts:44-51,78-94` | **Mismatch:** a 30-50 MB file passes the app's check and multer refuses it with 400 `{error:"File too large"}`; the app throws `body.error` (`composer/actions.ts:142-147`) and toasts the raw English "File too large", not the localised 50 MB message. | magpie (blob store) + app |
| Image upload route (legacy CloudCLI) | 5 MB, 5 files, JPEG/PNG/GIF/WebP/SVG | `assets.routes.ts:29-42,57-75` | 400 with multer's message. Not used by the Paseo app (images go inline). | gone |
| Download in the app | the download store refuses above "64 MB" | `paseo/packages/app/src/stores/download-store.ts:121` | Toast `fileTooLarge` with 64 MB. | app / magpie |
| Inline images per message | **no count limit**; each image is written to the assets dir and a model copy is made: long edge <= 2,000 px, <= 3,932,160 bytes (3.75 MiB, so base64 stays under the API's 5 MB), HEIC converted, JPEG quality stepping down to 55 | `claudecodeui server/modules/paseo/paseo-hub.ts:55-95`; `server/modules/jarvis/jarvis-images.ts:22-26,69-95` | Images are always made to fit one by one. See the WebSocket and /proc rows for the aggregate. | magpie (store once as blobs, previews) |
| WebSocket message | 100 MiB per Paseo frame (client `MAX_WEBSOCKET_MESSAGE_BYTES`; the `ws` server's default `maxPayload` is the same 100 MiB, the fork sets none); relay transport 32 MiB | `paseo/packages/protocol/src/websocket-message-limit.ts:2,5`; `paseo/packages/client/src/daemon-client.ts:1221-1228`; `server/modules/websocket/services/websocket-server.service.ts:84-96` | The app refuses to send: "Message is too large to send (X MiB). The limit is 100.0 MiB, so send fewer or smaller attachments." (`daemon-client-transport-utils.ts:137-141`, English only). | pigeon |
| /proc frame to the sandbox | 16 MiB per frame; larger -> the agent destroys the socket | `chat/sandbox/agent.mjs:159-172,237-242`; client `sandbox-process.ts:210-215` | **Code-derived:** the SDK writes each user message as one NDJSON line, so a message whose inline base64 images total > 16 MiB (about 4 photos at the 3.75 MiB model size, before the 100 MiB app check would stop anything) kills Claude Code: the run ends with "process exited", shown as `sandboxFailed` 「未能啟動這個對話的電腦…」 plus a failed-turn row. | supervisor (no /proc; images by reference) |
| Timeline page | 40 rows per page (app `TIMELINE_FETCH_PAGE_SIZE`; server default 40, clamped 1-500) | `paseo/packages/app/src/timeline/timeline-fetch-policy.ts:3`; `server/modules/paseo/paseo-timeline.ts:70,288` | Older rows load as the person scrolls. Behind it the server still loads the **whole transcript** (`paseo-hub.ts:214`, `fetchHistory(sessionId, {limit: null})`), which is why heap is the real limit. | magpie (`events?before=<seq>&limit=40`, design doc) |
| History cache | 256 MiB of transcript files, 8 entries per process | `server/modules/providers/services/session-history-cache.service.ts:56-61` | Re-read from JuiceFS on a miss. | gone |
| Conversation list | **unpaged**: every conversation in one `fetch_agents_response` / `fetch_workspaces_response` (`hasMore: false`) | `server/modules/paseo/paseo-connection.ts:619-642` | No limit today; grows with use. | magpie (needs a page size) |
| Recently deleted list | unpaged; search is a title substring filter | `paseo-connection.ts:644-652` | — | magpie |
| Conversation search route | default 50 results, max 100, at most 2 matches per conversation | `server/modules/providers/provider.routes.ts:392-406`; `services/session-conversations-search.service.ts:98,831,1148` | Results cut at the limit. | magpie |
| Projects sessions page | default 20, max 200 | `server/modules/projects/services/projects-with-sessions-fetch.service.ts:76-77,111-115` | CloudCLI surface; Paseo doesn't use it. | gone |
| Claude settings file read | 2 MiB | `server/modules/providers/services/claude-settings-permissions.service.ts:50,75` | Ignored above it. | gone |
| Claude Code transcript internals | 100 MiB append chunks; tombstone removal skipped for files > 50 MiB; compaction for files >= 5 MiB | Claude Code 2.1.304 (inventory "transcript append path", "in-place rewrites") | A > 50 MiB transcript can't drop a refused message. Becomes magpie's storage behind FUSE. | supervisor/magpie |
| Paths | 4,096 chars for workspace/sent paths | `jarvis-files.routes.ts:88`; `jarvis-sent-files.ts:40`; `router.mjs:395` | 400. | magpie |
| Files panel / sent files | no size or count cap on listing or download | `jarvis-files.routes.ts:69-80,148-170` | — | magpie |

## 10. App-side timing (Paseo protocol; pigeon must honour these)

| Limit | Value | Where | When hit |
|---|---|---|---|
| Reconnect backoff | 1.5 s doubling to 30 s | `paseo/packages/client/src/daemon-client.ts:1133-1134` | "reconnecting" state. |
| Connect timeout | 15 s | `daemon-client.ts:1137` | Retry. |
| Liveness | ping every 10 s, pong within 15 s, reconnect after 2 misses | `daemon-client.ts:1138-1141` | Reconnect. |
| RPC / send-queue timeout | 60 s | `daemon-client.ts:1135,1144` | A request with no answer fails at 60 s (inventory "Unsupported RPC error surface"). Any pigeon/magpie call behind an RPC must answer within 60 s (a cold sandbox start can take longer: it must be async with status frames, as today's `preparing`). |
| Server heartbeat | `ws` ping every 30 s | `server/modules/websocket/services/websocket-server.service.ts:20-22` | Dead sockets closed. |
| client_heartbeat | every 15 s | inventory (`hooks/client-activity-tracker.ts:1`) | Presence only. |
| Preparing chip | shown after 1 s of sandbox wait; "can take a minute or two" after 20 s | `sandbox-process.ts:85`; `yyc/sandbox-chip.tsx:12` | — |
| Chunk retry | 0.5 s, 1.5 s, then one reload per 10 min per tab | inventory (`utils/lazy-with-retry.ts`) | — |
| /api/jarvis/me | 3 s timeout, defaults on failure | inventory (`yyc/me.ts`) | No greeting name. |

## 11. Rollout drains and grace periods (zero-downtime bounds)

| Role | Drain | Grace | Where | What is cut beyond it |
|---|---|---|---|---|
| ui (router) | 900 s | 960 s | `ui.yaml:44,82` | a turn longer than 15 min during a rollout |
| web | preStop 10 s + 5 s + 300 s | 330 s | `jarvis3/deploy/chat/web.yaml:60,76`; `web/main.ts:84,97-98` | a sandbox start longer than 300 s |
| egress | 120 s | 150 s | `egress.yaml:25,49`; `egress/main.ts:100` | a model stream longer than 2 min (Claude Code retries; the cut call's cost is lost from the budget) and open pip/curl tunnels |
| tools | 150 s | 180 s | `tools.yaml:25,43`; `tools/main.ts:34` | nothing normally (upstream calls cap at 120 s), except via egress's 120 s on the same path |
| cloudflared | — | 200 s | `jarvis3/deploy/chat/cloudflared.yaml:26` | — |
| sandbox-vm gateway pod | — | 2 s, maxUnavailable 1 | YYC `yyc/deploy/sandbox-vm.yaml:22,32` | — |

## 12. Pod and namespace resources (today)

| Pod | Memory (request = limit) | CPU limit | Replicas | Where |
|---|---|---|---|---|
| ui | 1536Mi | 2 | 2 | `ui.yaml:107-108` (live: matches) |
| web | 256Mi (YYC patch) | 1 | 2 | YYC `kustomization.yaml:50-53`; `web.yaml:102-103` |
| egress | 256Mi | 1 | 2 | `egress.yaml:66-67` |
| tools | 128Mi (YYC patch) | 1 | 2 | `kustomization.yaml:61-64` |
| cloudflared | 64Mi (YYC patch) | — | 2 | `kustomization.yaml:65-68` |
| sandbox-vm gateway helper | 32Mi request / 96Mi limit | — | — | `sandbox-vm.yaml:56-57` |
| Namespace `jarvis` LimitRange | default request 50m / 64Mi / eph 64Mi; default limit 256Mi / eph 1Gi; max 4Gi / eph 16Gi per container | — | — | `jarvis3/deploy/chat/limits.yaml:10-12` |
| Postgres pool per process | 10 connections (`PG_POOL_MAX`), idle 30 s | — | — | `jarvis3 chat/src/common/db.ts:16` |
| Apps nodes | cx23 (2.7 GiB allocatable); UI on the 8 GB cx33/cpx32 nodes | — | — | `kustomization.yaml:46-47`; `juicefs.yaml:1-7,63` |

## 13. Claude Code's own defaults (not overridden today)

The sandbox sets no `BASH_DEFAULT_TIMEOUT_MS`, `BASH_MAX_TIMEOUT_MS`, `BASH_MAX_OUTPUT_LENGTH`,
`MAX_MCP_OUTPUT_TOKENS`, `MCP_TOOL_TIMEOUT` or `API_TIMEOUT_MS` (`chat/sandbox/Dockerfile:101-109`;
`chat/src/web/claude-code.ts:46-64`; the fork forwards only `CLAUDE_CODE_ENTRYPOINT`,
`CLAUDE_CODE_PRINT_BG_WAIT_CEILING_MS`, `MAX_THINKING_TOKENS`, `CLAUDE_CODE_MAX_OUTPUT_TOKENS`, `sandbox-process.ts:25`).
So Claude Code 2.1.304's documented defaults apply: Bash 2 min default / 10 min max per command, Bash output cut
at 30,000 chars, MCP tool output capped at 25,000 tokens, `cleanupPeriodDays` 30 (transcripts older than 30 days
deleted at startup: the inventory's "hidden delete" risk). The model sees a timeout or truncation notice in the tool
result. The supervisor should set these explicitly so they are a decision, not a default.

---

## 14. What the refactor has to decide (from the limits above)

1. **"Busy" is not reported as busy.** PoolFullError and "all hosts full" reach the UI as 500 and show the generic
   start failure. hermit's REST contract needs a distinct "no capacity" answer (with retry-after) and the app a
   message for it.
2. **Sandbox death mid-turn spins forever** (OOM at 1 GiB, 8 h lifetime, host loss). The supervisor owns the
   sandbox, so whoever watches it (hermit's lifecycle, magpie's turn) must end the turn with an error event within
   seconds, not wait for TCP. The 8 h lifetime should not kill a running turn.
3. **Workspace quota holes:** set whenever hermit provides /workspace (idempotent), not only on a new folder; decide a
   cap for magpie's per-conversation storage (today ~/.claude, shares and sent are unlimited).
4. **Upload caps disagree** (app 50 MB, server 30 MB, download 64 MB, WS 100 MiB, /proc 16 MiB per message, model
   body 64 MiB per call). One number for "a file you can attach", enforced by magpie's blob upload, shown by the app
   in its own language.
5. **Images in the model body:** every image is re-sent each turn, so the 64 MiB body cap ends long image-heavy
   conversations. Blobs by reference in magpie help the UI side; the model side is Claude Code's request shape and
   needs a decision (e.g. older images dropped or downscaled further).
6. **Unpaged lists:** conversation list and Recently deleted have no page size; magpie's API should page them like
   events (40).
7. **Concurrency today is 8 people** (4 processes x 2 pods). pigeon removes that limit; the remaining caps are
   `maxActive` 10 sandboxes, 12 slots per AWS host, 3 hosts, the shared yyc-mcp 120/min bucket and the USD 100
   shared budget.
8. **Budget is model cost only and global.** Tool costs (TikHub, AnyAPI, Exa) are logged but not budgeted, and cut
   streams lose their cost. Whether egress should count them stays egress's decision.
9. **Stale numbers in docs and comments:** SANDBOX-VM.md "an m7i.xlarge has 15" slots (12 by CPU); juicefs.yaml
   "320 Mi" mount pod (192Mi); README.md:234 example "30 美元" (limit is 100); claude-code.ts keeps the obsolete
   "check big writes" rule; `aws.emptyMinutes` is unused but still in config; the zh-HK i18n file still has the
   English `fileTooLarge`.

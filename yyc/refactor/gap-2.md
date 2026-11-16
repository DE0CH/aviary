# Gap 2: admin and operator workflows beyond the ops page

Researched 2026-11-02. Sources: DE0CH/YYC at 1775efc4, DE0CH/jarvis3 at 3f7499a (= the pinned base,
`yyc-ai/deploy/kustomization.yaml:9`), DE0CH/claudecodeui at 3de3f5d8 (= the live `ui` image,
`kustomization.yaml:33`), claude-env `.claude/skills/yyc-ai/*`, and read-only queries on the live cluster
(`kubectl get deploy`, SQL through a web pod). YYC paths are relative to the YYC repo, jarvis3 paths to jarvis3,
fork paths to claudecodeui, `skill/` = `claude-env/.claude/skills/yyc-ai/`.

Legend for the "kind" column, against the target design's rules:
- **GIT-CFG**: a value in `yyc-ai/deploy/config.json` (shared ConfigMap `jarvis-config`), applied by a push and a pod
  roll. Fine only where the target owner is a YYC implementation (hermit, egress, tools); **forbidden** where today's
  reader becomes pigeon, magpie or supervisor (they must not carry deployment config).
- **SQL-ONLY**: done by hand-written SQL on database `jarvis`; no API, no audit. The target needs an admin REST route
  on the owner (services talk only REST; each owns its DB behind its API).
- **SCRIPT**: an idempotent script run by hand from a checkout with store keys.
- **CODE**: a constant in code; changes need a release.
- **UI**: an app/ops-page action.

## 1. Summary

| # | Workflow | How today | Kind | Target owner |
|---|---|---|---|---|
| 1 | Add/remove staff, admins | `users`/`admins` in config.json, push; Flux rolls; tofu rebuilds Access policies; yyc-mcp re-reads | GIT-CFG (+tofu) | hermit ("who may use"; roles) |
| 2 | Budget reset / raise | `budget.since` / `budget.limitUsd` in config.json, push, roll | GIT-CFG | egress |
| 3 | Model switch (work, title, context window, reasoning) | config.json `models`/`defaultModel`/`titleModel`/`titleReasoning`; `infra/openrouter.py` first; `test_zdr.py` | GIT-CFG + SCRIPT | egress (model list) — supervisor/magpie must *ask* `model` |
| 4 | System prompt | `deploy/system-prompt.md` + jarvis3 `SANDBOX_RULES`, push | GIT-CFG + CODE | hermit (YYC content) → supervisor writes it |
| 5 | Tool allowlist (yyc-mcp) | `tools.upstreams[0].tools` in config.json | GIT-CFG | tools |
| 6 | Sandbox internet tweaks | `egress.denyHosts`/`defaultAllowedHosts`; jarvis3 generated list + weekly workflow | GIT-CFG + CI | egress |
| 7 | Cap / drain / retire a host | `update vm_hosts set max_sandboxes=…/status='retiring'` | **SQL-ONLY** | hermit (admin REST) |
| 8 | Force a burst / provoke a wait (test) | SQL on `vm_hosts.max_sandboxes` | **SQL-ONLY** | hermit |
| 9 | Turn sandboxes off | SQL `max_sandboxes=0` on every host + config `aws.maxHosts: 0` | SQL-ONLY + GIT-CFG | hermit |
| 10 | Capacity (more hosts, slots, idle, lifetime) | config `sandbox.*` | GIT-CFG | hermit |
| 11 | View as (read / send as a person) | ops page button → router `/api/jarvis/view-as` → HMAC token; audit `ops_events` | UI | pigeon (token, gate) + magpie (enforce, audit) |
| 12 | Read one person's conversation without the UI | SQL on `cloudcli.cc_<key>` + JSONL on `/jfs/users/<key>/claude` from a ui pod | SQL/FS | magpie (admin read API) |
| 13 | Debug a chat | `kubectl logs`, SQL on `sandboxes`/`egress_log`/`tool_calls`/`ops_events`/`vm_events`, host `journalctl` | SQL/logs | each service's logs + read APIs |
| 14 | Synthetic chat / checks | `skill/ui-pod-chat.sh`, `view-as-check.mjs`, `share-viewer-check.mjs`, … (spawn a per-person CloudCLI in a ui pod) | SCRIPT | all must be rewritten (section 3) |
| 15 | Reseal a secret + `yyc/secrets-rev` bump | store → `infra/seal.py jarvis/<name>` → bump annotation → push | SCRIPT + GIT | ops (unchanged mechanism, new Secret names) |
| 16 | Rotate `JARVIS_CC_TOKEN` | reseal web-env + ui-env + yyc/monitor-keys at once; single-valued | SCRIPT | disappears; replaced by per-service tokens |
| 17 | Rotate `JARVIS_SENT_FILES_TOKEN` | reseal egress-env + ui-env | SCRIPT | disappears (snapshots move into magpie) |
| 18 | Rotate sandboxd PKI | `chat/sandboxd/host/make-certs.sh`, store, reseal `sandbox-vm`, gateway `/etc/sbx/tls` | SCRIPT (no runbook) | hermit |
| 19 | Rotate scaler / AMI-CI AWS keys | `aws/infra.sh iam-key` / `ci-iam-key` | SCRIPT | hermit |
| 20 | OpenRouter key: guardrail, cap, rotate, verify | `yyc-ai/infra/openrouter.py [--rotate|--finish-rotate|--verify]` | SCRIPT | egress (ops tooling) |
| 21 | AMI bake / template / prune / check | `aws/infra.sh ami|template|sync|check|current|prune-amis`, workflow `yyc-ai-ami` | SCRIPT + CI | hermit |
| 22 | Gateway install / upgrade / move | `host/fetch-sandboxd.sh` + `host/gateway.sh` as root on the node | SCRIPT | hermit |
| 23 | sandboxd / runsc upgrade | CI release → new AMI → cap old hosts | SCRIPT + SQL | hermit |
| 24 | Deploy a new version / rollback | bump refs/tags in `deploy/kustomization.yaml` + `config.json sandbox.image`, push | GIT | ops |
| 25 | Take YYC AI offline | point tofu `staff_ai` at the admins policy | GIT (tofu) | front door (gap 1) |
| 26 | Back to the old chat UI | pin jarvis3 2156d8a, tunnel → web | GIT | drop (clean cut) |
| 27 | Switch to E2B | config `sandbox.provider: "e2b"` + keys + tofu | GIT-CFG behaviour switch | hermit implementation choice, or drop |
| 28 | gVisor on a cluster node | `yyc-ai/infra/gvisor-node.sh` | SCRIPT (inert) | drop |
| 29 | Live dashboard feed | news-stream monitor polls `/internal/ops/snapshot` with `JARVIS_OPS_TOKEN` (= `JARVIS_CC_TOKEN`) | contract | the ops snapshot's new owner |
| 30 | iOS app record / builds | `skill/ios-app-record.sh`, fork workflow `yyc-ios` | SCRIPT + CI | pigeon's client (unchanged) |

## 2. Each workflow in detail

### 2.1 Staff and admins (allowlist)

- **Today:** `users` and `admins` in `yyc-ai/deploy/config.json:5-6`. They are the access catalogue of every YYC app,
  not only YYC AI (`yyc-ai/README.md:254-261`). Edit, `git pull --rebase`, push. Five readers:
  1. Cloudflare Access policies `YYC users` / `YYC admins` (tofu reads the file:
     `news-stream/deploy/cloudflare/tofu/access.tf:53-66, 94-101`, applied by `.github/workflows/cloudflare.yml`);
  2. the ui router, read **once at start** (`fork jarvis/router.mjs:583-596`; allow check `:865-871`, 403 `:883`);
  3. web, read once at start (`jarvis3 chat/src/web/main.ts:74` → `common/config.ts:332-333`): `/internal/cc/sandbox`
     and `warm` 403 unless in `users` (`docs/CHAT.md:44-55`), `view-as` and `/internal/ops` 403 unless in `admins`
     (`web/app.ts:119, 144`);
  4. yyc-mcp in namespace `yyc` via ConfigMap `yyc/yyc-catalogue` (fixed name, re-read every 30 s;
     `yyc-ai/deploy/kustomization.yaml:38-43`);
  5. tools: `config.admins[0]` is the identity tools uses to list upstream tools (`jarvis3 chat/src/tools/main.ts:21-22`,
     refresh every 10 min `tools/upstream.ts:153-160`) — an admin-list side effect nobody documents.
- Because the ConfigMap `jarvis-config` is hashed, any edit of config.json (a user, a budget, a model) rolls **web,
  egress, tools and ui** (`yyc-ai/README.md:89-90`). The ui roll waits for running chats (up to 900 s drain,
  `deploy/ui.yaml`), so a one-line allowlist edit costs a full rolling deploy of all four.
- **Offboarding:** remove from `users`. Former users with sandboxes still appear on the ops page "people"
  (`jarvis3 web/ops.ts:490-519`); their data is untouched (deletion is gap "Deletion, purge and offboarding").
- **Both layers must agree:** Access lets the address sign in, the router checks `users` (`skill/SKILL.md:83-85`);
  `"@yyc.hk"` = Access `email_domain` and the router's suffix rule (`README.md:43`).
- **Stale doc:** `skill/SKILL.md:83` says "re-run `cloudflare.py` (no `--dns`)"; cloudflare.py was deleted
  (YYC 29660229, gap 1); tofu now follows the push by itself. The tracked bytecode
  `yyc-ai/infra/__pycache__/cloudflare.cpython-312.pyc` is still in git (`git ls-files yyc-ai/infra`).
- **Target:** hermit owns "who may use the deployment" and must also answer *roles* (admin vs staff): pigeon asks
  hermit at sign-in and per request (cacheable), magpie asks it for view-as checks, the ops view asks it for "people".
  The YYC hermit can keep reading the catalogue file, but it should re-read it (like yyc-mcp, 30 s) instead of
  needing a roll; pigeon/magpie must not mount config.json at all. tools' listing identity must come from tools' own
  config (tools is YYC's), not from `admins[0]`.

### 2.2 Budget reset / raise

- **Today:** `budget: {limitUsd: 100, since: "2026-10-21T02:05:52Z", ownerName: "Deyao"}` (`config.json:67`).
  Reset = `since` = now UTC; raise = bigger `limitUsd`; push; pods roll (`README.md:235-238`, `docs/CHAT.md:487-497`).
  Gate: egress reads it at start (`jarvis3 chat/src/egress/main.ts:17, 44`), spend =
  `sum(egress_log.cost_usd) where kind='model' and at >= since` (`common/budget.ts:20-37`). Only when Deyao says so.
- **Live:** 1,802 model calls, USD 25.66 since `since` (query 2026-11-02).
- **Removed path:** an admin override in kv `budget:override` with an `ops_events` `budget` audit row existed
  (jarvis3 3076ec3) and was removed; 13 `ops_events` rows of kind `budget` remain (live count), no kv key.
- **Stale docs:** README says USD 100 in one place (`README.md:231`) but the refusal text example says 30 美元
  (`README.md:234`); `skill/SKILL.md:109-114` and `jarvis3 docs/CLAUDE-CODE.md:205` still say USD 30. The real message
  is built from `limitUsd` (`docs/CHAT.md:493`).
- **Backstop:** OpenRouter key cap USD 50/month in `yyc-ai/infra/openrouter.py:25`, set at key creation/patch
  (`:93, :100`).
- **Target:** egress (YYC gateway implementation) keeps the gate and the ledger. Nothing in pigeon/magpie may hold the
  limit; they only render egress's 402 (fork `jarvis-errors.ts:68, 106-108` today). A reset is a config value of egress,
  which is allowed (egress is YYC's implementation); optionally an admin REST route on egress so a reset doesn't roll
  every service.

### 2.3 Model switch

- **Today:** `config.json:7-12` (`models[]` with `id`, `label`, `reasoning`, `contextWindow`; `defaultModel`,
  `titleModel`, `titleReasoning`). Live: DeepSeek V4.1 Flash for both work and titles.
- Procedure (`skill/SKILL.md:86-99`; `README.md:204-227`):
  1. only models with OpenRouter ZDR endpoints: run `python3 yyc-ai/tests/test_zdr.py` locally first, because Flux
     applies a push without waiting for CI (`README.md:215-217`; test `yyc-ai/tests/test_zdr.py:1-40`, CI
     `.github/workflows/yyc-ai-zdr.yml`);
  2. run `YYC_OPENROUTER_MANAGEMENT_KEY=… yyc-ai/infra/openrouter.py OUT` **before** the pods roll (the guardrail's
     `allowed_models` come from the local working tree's config.json, `openrouter.py:27-28, 103-125`), else calls
     get OpenRouter 404 "Model blocked by guardrail" (~1 min to apply);
  3. switch without breaking live chats: list old + new, run openrouter.py, deploy, drop old, run it again;
     `--verify` checks the key live (`openrouter.py:41-69`).
- Who reads what: egress enforces the shortlist (`egress/proxy.ts:582-591`) and per-model reasoning
  (`egress/modelbody.ts:46-55, 70-82`); web turns `defaultModel`/`titleModel`/`contextWindow` into Claude Code env
  `ANTHROPIC_MODEL`, `ANTHROPIC_SMALL_FAST_MODEL`, `CLAUDE_CODE_MAX_CONTEXT_TOKENS` (`web/claude-code.ts:49-53`);
  the fork shows the label read-only (`paseo/packages/app/src/yyc/model-label.tsx:11-69`, "the admin sets it" `:36`)
  and the title model runs from the fork (`server/modules/jarvis/jarvis-title.ts`).
- **Finding (verify weakened):** `openrouter.py --verify` picks a model containing "haiku", else `MODELS[0]`
  (`:44`), and its two "must be refused" ZDR cases force `provider.only: ["anthropic"]` (`:46-53`). With DeepSeek as
  the only model, those requests fail because no Anthropic endpoint serves DeepSeek, not because of the guardrail, so
  the check passes without testing ZDR. It never tries DeepSeek's own non-ZDR endpoint, which the skill says must never
  be used (`skill/SKILL.md:95`).
- **Stale doc (fixed 2026-11-04):** `README.md` "Model and budget" now states the live models from `deploy/config.json`.
- **Target:** the model list (ids, labels, context window, reasoning, which is the title model) belongs to the `model`
  endpoint (egress for YYC). The supervisor must get `ANTHROPIC_MODEL`/small-fast/context window from that endpoint
  (e.g. a `GET model/…/models` answer), and magpie's title call and pigeon's model label likewise, so none of them
  carries a model name. A model switch then = an egress config change + openrouter.py, nothing else.

### 2.4 System prompt (managed CLAUDE.md)

- **Today:** `yyc-ai/deploy/system-prompt.md` (config `systemPromptFile`, `config.json:13`) + jarvis3's
  `SANDBOX_RULES` (`chat/src/web/claude-code.ts:13-35`), sent base64 as `JARVIS_CLAUDE_MD` (`:61`), written to
  `/etc/claude-code` by `sbx-init.mjs`. Edit, push; new sandboxes only (`README.md:89-90`, `skill/SKILL.md:121-123`).
  `SANDBOX_RULES` carries YYC-shaped facts (the 1 GiB / 1,000-file quota, the temporary "check big writes" rule).
- **Target:** prompt content is deployment-specific, so it comes from hermit (YYC implementation) as part of what a
  sandbox is given; the supervisor writes it to the managed path the same way in every deployment. The quota rule
  text should be generated from what hermit actually provisions for /workspace.

### 2.5 Tool allowlist (yyc-mcp)

- **Today:** `config.json:57-64` `tools.upstreams[0].tools` (explicit read-only list); tools hides listed tools marked
  `readOnlyHint:false`/`destructiveHint:true` anyway (`jarvis3 chat/src/tools/upstream.ts:149`). Check hidden tools:
  `kubectl -n jarvis logs deploy/tools | grep "upstream tools"` (`README.md:157-163`). `X-YYC-User: ${USER_EMAIL}`
  (`config.json:59`) is the sandbox owner, resolved from the egress token.
- **Target:** tools, unchanged. The owner email must now come from hermit (sandbox → owner), not from web's
  `sandboxes` table.

### 2.6 Sandbox internet tweaks

- `egress.denyHosts` (wins), `egress.defaultAllowedHosts`, `generatedAllowlist` (`config.json:37-40`); jarvis3's
  `chat/egress-allowlist/` refreshed weekly by `.github/workflows/egress-allowlist.yml`, taken by a pin bump
  (`README.md:164-173`, `skill/SKILL.md:66-73`). Denials in `egress_log.decision`.
- **Target:** egress, unchanged.

### 2.7 Host operations (cap, drain, retire) — SQL only

- `update vm_hosts set max_sandboxes = N where name = …` (0 = no new; null = by memory); `status = 'retiring'`
  empties a host (sandboxes move off, one per pass, longest idle first; a transient extra host if needed); the scaler
  acts within ~5 s (`skill/SKILL.md:157-160`; `jarvis3 docs/SANDBOX-VM.md:240-245`). Code: slots
  `web/sandbox-vm.ts:191`, retiring cap +1 `:318-330`, retiring need `:347-352`, move-off loop `:643-670`.
- The scaler also sets `retiring` itself when JuiceFS stays unmounted (`sandbox-vm.ts:569-588`), so an operator's
  `retiring` and the system's look the same; there is also a `draining` status (`:51`).
- No audit: nothing records who capped a host. No API: `vm_hosts` is web's table.
- **Live:** `vm_hosts` has 2 non-gone rows: `yyc-sbx-gw` kind `hub` (the gateway, which runs no sandboxes) and one AWS
  host; 199 `gone` rows are kept forever.
- **Target:** hermit admin REST, e.g. `PATCH hermit/hosts/{name} {max_sandboxes, status}` with an audit row; the ops
  view shows the operator's intent separately from the system's.

### 2.8 Force a burst / provoke a wait (tests)

- `docs/SANDBOX-VM.md:243-245`: "hub `max_sandboxes = 0`, then back to null". **Obsolete:** since 2026-10-21 the hub
  is a gateway with no sandboxes (`docs/SANDBOX-VM.md:53`, `README.md:136-140`).
- The current trick (`skill/SKILL.md:326-327`): set each active AWS host's `max_sandboxes` to its current count so
  the next sandbox needs a new host, chat, set back to null. SQL only; forgetting to set it back silently caps
  capacity.
- **Target:** a hermit admin call ("next sandbox on a fresh host" or a TTL'd cap) so a test can't leave a cap behind.

### 2.9 Turn sandboxes off

- `skill/SKILL.md:338`: `max_sandboxes = 0` on every host and config `aws.maxHosts` 0 (`config.json:26`). Mixed SQL +
  config + roll. Not in the README. New conversations then fail with "all sandbox hosts are full"
  (`skill/SKILL.md:173-174`), i.e. staff get an error rather than a maintenance message.
- **Target:** one hermit switch (admin REST) whose refusal reason magpie/pigeon can show as such.

### 2.10 Capacity knobs

- `config.json:16-27`: `idleMinutes` 30, `maxLifetimeHours` 8, `maxActive` 10 (`web/sandbox.ts:177`), `cpu`, `memory`,
  `spareSlots`, `minHostLifetimeSeconds`, `aws.maxHosts` 3 (16-vCPU quota, +1 transient), `emptyMinutes`,
  `cpuFeatures`, `hub.*`. Instance tag `aws.instance`/`e2b.instance` = "ai.yyc.deyaochen.com" is an identity, not an
  address (`README.md:60`).
- **Target:** all hermit (YYC implementation); allowed there.

### 2.11 View as

- **Start:** ops page 「View as」 (`jarvis3 web/ops-page.ts:273`) = form POST to router `/api/jarvis/view-as`
  (`fork router.mjs:103, 965, 1023-1052`); admin-only (`:1029`), records `start` in web first
  (`:1054-1064` → `web/app.ts:110-131` → `ops_events` kind `view_as`), 303 to `/#jarvis-view-as=<token>`.
- **Token:** HMAC key derived from `JARVIS_CC_TOKEN` (`router.mjs:623-627`), 12 h (`JARVIS_VIEW_AS_HOURS`, `:84`),
  honoured only for the same admin still in `admins` (`:875-891`); else 403 `VIEW_AS_REFUSED` (`:960`).
- **Read-only:** REST GET-only plus warm and uploads (`router.mjs:109, 975-979`); sockets only `/paseo-ws`, whose
  bridge records each send before sending and refuses renames/deletes/restores (`server/modules/jarvis/jarvis-view-as.ts:28-52`;
  `server/modules/paseo/paseo-connection.ts:111, 136-141, 375-398, 412-413, 429-430, 458-472, 820-845`).
  Client: `paseo/packages/app/src/yyc/view-as.ts:13-99`, `view-as-box.tsx:14-50` (English override box).
- **Audit:** `ops_events` kind `view_as`, kept for good; live count 39 (includes throwaway test identities, which
  `view-as-check.mjs` and `view-as-browser-router.mjs` leave behind by design, `skill/view-as-check.mjs:9`,
  `view-as-browser-router.mjs:10`).
- **Target:** pigeon signs/validates the token and blocks writes; magpie enforces read-only on its own API (refuse
  rename/delete/restore with a viewer) and records the view and each send. Since magpie owns events, a send-as is an
  event row carrying the viewer; the audit list must survive (import the 39 rows). The HMAC key must become pigeon's
  own secret, not derived from a token shared with other services.

### 2.12 Reading one person's conversation outside the UI

- Today only through view-as or by hand: `cloudcli` schema `cc_<userKey>` (userKey = `u` + sha256(email)[:24],
  `skill/ui-pod-chat.mjs:10`) and transcripts at `/jfs/users/<key>/claude/projects/-workspace/<session>.jsonl` from a
  ui pod (`skill/SKILL.md:119-120, 303-305`). "Find what a chat's agent tripped over" = grep `is_error` in that JSONL.
- **Target:** magpie admin read (events by conversation, with the admin's identity audited like view-as). Without it,
  debugging needs direct DB access to magpie's Postgres, which breaks "each owns its DB behind its API".

### 2.13 Debugging a chat

- Logs: `kubectl -n jarvis logs deploy/web|egress|tools|ui` (web `sandbox ready`/`released`/`vm: …`; ui: router +
  CloudCLI) (`skill/SKILL.md:299-300`).
- SQL on `jarvis` via `news-stream/deploy/cluster/cluster.sh db-forward` (`postgresql://jarvis:$YYC_PG_JARVIS_PASSWORD@127.0.0.1:15432/jarvis`)
  or node+pg in a web pod: `sandboxes` (owner, status, release_reason), `egress_log`, `tool_calls`, `ops_events`,
  `vm_events` (`skill/SKILL.md:301-305`).
- Hosts: `journalctl -u sandboxd`, `/var/log/sbx/<sandbox>.log` (`skill/SKILL.md:155-156`).
- List problems: ui logs `broadcast failed` / `listen connection lost` (`skill/SKILL.md:311-316`); sent files: router
  log `"sent files kept"`, `find /jfs/sent/<conversation>` (`skill/SKILL.md:194-195`); tools: `upstream tools`.
- Isolation checks: direct `GET /proc` upgrade against the agent with the exec token from a ui pod
  (`skill/SKILL.md:182-185`), because the model refuses probing prompts.
- **Live hygiene finding:** kv has ~150 leaked `presence:<role>:<pod>` and `replica:web_<pod>` keys from old pods and
  22 `turn_sandbox:<uuid>` keys from the old chat UI; nothing prunes them.
- **Target:** each service's logs keep stable messages; magpie (conversation, events, seq), hermit (sandbox, host,
  lifecycle events) and egress (calls, costs, denials) each need a read API keyed by conversation id so one
  conversation can be traced across them without SQL. A shared correlation id (conversation + turn) in every log line.

### 2.14 Synthetic chats and checks

See section 3: every script there drives a per-person CloudCLI process inside a ui pod and dies with it.

### 2.15 Resealing secrets and the secrets revision

- New value into store `yyc` (`jarvis put-key yyc KEY --explain …`), `infra/seal.py jarvis/<name>` (`--stdin KEY` for
  a value not in env), commit, push (`skill/SKILL.md:124-134`; `infra/seal.py:1-20`; map `infra/secrets.txt:21-28`).
- A Secret change doesn't roll pods: bump `yyc/secrets-rev` (`yyc-ai/deploy/kustomization.yaml:74-83`, now
  `2026-10-28-sentfiles-2`), because "a kubectl rollout restart is undone by Flux" (`:75`). **Contradiction:**
  `skill/SKILL.md:130` says `kubectl -n jarvis rollout restart deploy/<role>`. Pitfall: pods can start before the
  controller decrypted a resealed Secret, so check the key inside the pod (`skill/SKILL.md:196-198`; bumped twice on
  2026-10-28). A new key needs an `env` entry too where Deployments name keys individually (ui, egress patch
  `kustomization.yaml:54-60`).
- **Stale doc:** `README.md:267` and `infra/seal.py:14` say `jarvis2 put-key`; CLAUDE.md says
  `scripts/store-put.py`. `README.md:273-292` misses `WEB_FETCH_TOKEN`, `ANYAPI_API_KEY` (tools-env) and
  `JARVIS_SENT_FILES_TOKEN` (egress-env, ui-env) that `infra/secrets.txt:21-27` has.
- **Target:** same mechanism; the Secret names change with the components (no `web-env`/`ui-env` left). The
  "secrets-rev" annotation should be per Deployment so a reseal rolls only its readers.

### 2.16 Rotating JARVIS_CC_TOKEN

- One value, four jobs: UI → web bearer (`web/app.ts:172-177`, single value compared, no second accepted token);
  the view-as HMAC key (`router.mjs:626`); the router's peer key (`router.mjs:33`); and the news-stream monitor's
  `JARVIS_OPS_TOKEN` (`infra/secrets.txt:13`, `skill/SKILL.md:222-224`). Lives in `web-env`, `ui-env`,
  `yyc/monitor-keys`. Also forwarded to every per-person CloudCLI process (`router.mjs:492`;
  `server/modules/jarvis/sandbox-process.ts:38`, `jarvis-warm.routes.ts:20`, `jarvis-view-as.ts:38`).
- No runbook. Rotation as built is not zero-downtime: while web and ui roll at different times, half the replicas
  hold the old value and get 401; all open view-as tabs die.
- **Target:** gone with web/ui. Replace with one credential per caller→callee pair, each callee accepting a current
  and a next value during a rotation.

### 2.17 Rotating JARVIS_SENT_FILES_TOKEN

- egress injects it on route `sent-files` (`config.json:46-47`; `kustomization.yaml:54-60`), router checks it
  (`router.mjs:619`). Store `YYC_JARVIS_SENT_FILES_TOKEN`. No runbook; single-valued.
- **Target:** disappears: sent-file snapshots belong to magpie (design), reached at the fixed `magpie` endpoint.

### 2.18 Rotating the sandboxd PKI

- `jarvis3 chat/sandboxd/host/make-certs.sh:1-30`: CA (10 y, CN "YYC AI sandboxd CA"), leaves `hub`, `aws`,
  `controller` (3 y). One `aws` cert shared by every AWS host, handed in user data (`SBX_HOST_CERT/KEY` in
  `sandbox-vm` Secret, `infra/secrets.txt:26`); CA key and hub cert only in the store (`README.md:292`); gateway reads
  `/etc/sbx/tls/{ca.crt,host.crt,host.key}` (`host/gateway.sh:14, 41`).
- No rotation procedure anywhere; no revocation (a shared leaf). Expiry ~2029 for leaves.
- **Target:** hermit (YYC). Write the rotation runbook when it moves.

### 2.19 AWS keys (scaler, AMI CI)

- `aws/infra.sh iam` / `iam-key` (scaler user `yyc-ai-sandbox-scaler`, policy `aws/scaler-policy.json`), `ci-iam` /
  `ci-iam-key` (AMI CI user `yyc-ai-sandbox-ami-ci`, `aws/ami-ci-policy.json`); keys written to `$KEY_OUT`
  (`aws/infra.sh:24-30, 159-183`). Scaler key → store `YYC_SBX_AWS_*` → `sandbox-vm` Secret; CI key → YYC GitHub
  secrets `YYC_AI_AMI_AWS_*` (`docs/SANDBOX-VM.md:235-240`).
- **Target:** hermit; unchanged.

### 2.20 OpenRouter key and guardrail

- `yyc-ai/infra/openrouter.py`: creates/patches key `yyc-chat` (USD 50/month, `:25, 90-101`), guardrail `yyc-chat`
  (ZDR for every group, no training/publication, `allowed_models` from config.json, `:103-125`), `--rotate` makes
  `yyc-chat-next` (`:85-88`), `--finish-rotate` deletes the old and renames (`:77-84`), `--verify` (section 2.3).
  Needs `YYC_OPENROUTER_MANAGEMENT_KEY` (also in `yyc/monitor-keys`, `infra/secrets.txt:13`).
- Rotation runbook is implicit: `--rotate` → store new key as `YYC_AI_OPENROUTER_KEY` → reseal `egress-env` → bump
  secrets-rev → push → `--finish-rotate`. Not written down.
- **Target:** egress's tooling; it reads the model list from egress's own config (no longer the shared file).

### 2.21 AMI tooling

- `jarvis3 chat/sandboxd/aws/infra.sh` (header `:1-31`): `sg`, `ipv6`, `ami <image>` (needs GHCR_USER/GHCR_TOKEN; from
  a CLEAN checkout: it copies the working tree, `skill/SKILL.md:161`), `template <ami>`, `current`, `check <image>`
  (exit 1 when stale), `sync <image>` (bake only the image, same sandboxd/runsc, then template + prune; `:202-212`),
  `prune-amis`, `iam`, `ci-iam`, `*-key`. YYC constants hard-coded in jarvis3: `NAME=yyc-ai-sandbox-host`, subnet
  `subnet-009eab323e09d6338`, tag `app=yyc-ai-sandbox` (`:35-37`).
- Workflow `YYC/.github/workflows/yyc-ai-ami.yml`: on every push touching `config.json` (so also allowlist and
  budget edits) runs `infra.sh sync` with **jarvis3 `main`**, not the pinned ref (checkout without `ref`), so the
  bake script can differ from the pinned base.
- **Target:** hermit (YYC implementation), with the YYC constants as its config; the workflow should trigger on the
  sandbox image pin only and use the pinned hermit version.

### 2.22 Gateway install / upgrade / move

- `host/fetch-sandboxd.sh DIR` (release `sandboxd-<sha12>`, SHA256SUMS checked, `:1-20`), then `host/gateway.sh
  [image]` as root on node `yyc-pool-apps-worker2` (`:1-99`): installs crane, sandboxd, `/etc/sbx/host.json`, unit
  with `MemoryMax=256M`, `Restart=always`; restart never touches wg0/nft (`:72-75`). SSH `root@2a01:4f8:c013:c93e::1`
  with `~/.yyc-cluster/admin` (`skill/SKILL.md:148-153`). Moving: same `/etc/sbx` on another node, re-point Hetzner
  route 10.200.0.0/15, firewall `yyc-sbx-gw`, and config `sandbox.vm.hub.endpoint`. Servers in project yyc are
  created/deleted by hand only.
- Outside GitOps (a systemd unit on a k3s node); not zero-downtime by design (restart keeps peers, but the control
  plane on :7443 is down for the restart).
- **Target:** hermit (YYC). The rule "GitOps via Flux" is not met here today; say so or move it into a DaemonSet.

### 2.23 sandboxd / runsc upgrade

- CI `sandboxd` publishes releases; AWS: new AMI (`infra.sh ami`, `SANDBOXD_RELEASE=`, `GV=`), `template`,
  `prune-amis`; cap old hosts with `max_sandboxes = 0` (SQL) because restore refuses another gVisor release, so old
  hosts empty as sandboxes are released (`docs/SANDBOX-VM.md:217-225`; `skill/SKILL.md:161-170`). Gateway: section
  2.22.
- **Target:** hermit; the SQL step becomes the hermit admin route of 2.7.

### 2.24 Deploy a new version / rollback

- jarvis3: pick a `main` commit whose `chat` workflow passed, set `?ref=` and `jarvis3-chat` `newTag`, and
  `sandbox.image` (+ `e2b.template`) in config.json; run `test_zdr.py`; push; watch `yyc-ai-ami`
  (`README.md:74-90`; `skill/SKILL.md:77-81`). Fork: push, wait `jarvis-image` (~25 min), set `yyc-ai-ui` tag. Rollback =
  the previous pins (`README.md:52`; `skill/SKILL.md:333`).
- **Target:** the same shape with aviary images; the sandbox image pin should stop living in the shared config file.

### 2.25–2.28 Offline, old UI, E2B, gVisor node

- Offline: tofu `staff_ai` → admins policy (`README.md:118-120`). Stale alternative in `skill/SKILL.md:334-335`
  (`yyc-ai/edge`, `UNDER_CONSTRUCTION`, `cloudflare.py --dns`) and `jarvis3 docs/CLAUDE-CODE.md:194-201` — both
  deleted mechanisms (gap 1).
- Old chat UI: pin jarvis3 2156d8a, tunnel → web (`README.md:121-123`; `skill/SKILL.md:336-337` still says
  `UI_CUTOVER = False`). Under the clean-cut rule this goes once the import into magpie is done.
- E2B: `README.md:175-198`, a `sandbox.provider` switch in config (`config.json:15, 29-35`), plus e2b keys, a tofu
  host and Access service token. That is a behaviour switch; in the target it is at most another hermit implementation.
  Not live; `web-env` lacks the keys.
- `yyc-ai/infra/gvisor-node.sh` (install/activate/check runsc on a cluster node; header refers to the removed
  `jarvis-sandbox` namespace and a README section that no longer exists). Inert; drop with the RuntimeClass.

### 2.29 Live dashboard feed (cross-namespace)

- news-stream monitor polls web `/internal/ops/snapshot` every 10 s (NetworkPolicy `web-from-news-monitor` in
  `deploy/ops.yaml`; token `JARVIS_OPS_TOKEN` = `JARVIS_CC_TOKEN`, user chendeyao000@gmail.com who must stay in
  `admins`; `skill/SKILL.md:222-224`; `news-stream/monitor.py:1885-1908, 2096-2131` per the inventory). Removing
  Deyao from `admins` silently breaks the dashboard.
- **Target:** whoever serves the ops snapshot keeps the URL, auth and field names, or monitor.py changes in the same
  commit.

### 2.30 iOS app operations

- `skill/ios-app-record.sh` (one-time record + group "Owner"; done 2026-10-22) still triggers
  `DE0CH/pwa-shell-ios` `ios.yml` (`:20`), the replaced SwiftUI shell; the live pipeline is the fork's `yyc-ios`
  workflow (`skill/SKILL.md:258-264`). `YYC_SITE` is a claudecodeui Actions secret.
- **Target:** unchanged client; the app's server URL stays a build secret.

## 3. Scripts in `claude-env/.claude/skills/yyc-ai/`

All but two depend on today's per-person CloudCLI process, `/jfs` in the ui pod, `cc_<key>` schemas or the
router's exported `Router` class. None survives the switch unchanged.

| Script | What it does | Depends on | In the target |
|---|---|---|---|
| `ui-pod-chat.sh` (23) + `ui-pod-chat.mjs` (76) | real chat from inside a ui pod: spawns `/app/dist-server/server/index.js` with `DATABASE_SCHEMA=cc_<key>`, `HOME` → `/jfs/users/<key>/claude`, `JARVIS_CC_TOKEN` (`ui-pod-chat.mjs:10-21`), chats on `/ws`, checks history+files, deletes; `KEEP`, `WARM_S`, `ATTACH` (`ui-pod-chat.sh:1-23`) | per-person process, `/ws`, `/jfs`, web | rewrite as a client of pigeon (Paseo protocol) or magpie REST with a test identity; this is the main smoke test |
| `view-as-check.mjs` (106) | second `Router` in a ui pod with a throwaway Access team (own JWKS), real `admins[0]` and a throwaway `view-as-check-<rand>@yyc.hk`; checks refusals, read-only REST, `/ws` refused, `/paseo-ws` renames refused, send recorded before it goes (`:1-9`) | `import { Router } from '/app/jarvis/router.mjs'`, injected JWKS, `cc_` schemas, `/jfs` | port to pigeon+magpie; needs a way to run pigeon against a test JWKS (a test build or test harness, not a deployment switch) |
| `view-as-browser-router.mjs` (73) | second router serving a `dist/` under test on 127.0.0.1:18090, prints both identities' cookies; cleans up schema, folders and listed conversations (`:1-10`) | same as above + `/tmp/va-app` copy of `/app` | pigeon serves the built app; build-under-test needs its own pigeon instance |
| `view-as-browser-test.mjs` (132) | Chrome: staff makes a conversation; admin opens ops page, 「View as」, list/read-only/override checks, admin tab untouched | the router above, the ops page | keep (client-side), point at the new test pigeon |
| `share-viewer-check.mjs` (47) | as a throwaway viewer process: open share, download sent file, refused file, 繼續對話, history, cleanup (`:1-30`) | per-person process, `cc_shared.shares`, `/jfs/users/<owner>/shares` | magpie shares API |
| `typable-first-test.mjs` (475) | early message box: typing, geometry, overlay, reply rendering (`:1-21`) | a test router | keep; client test |
| `load-timing.mjs` (155) | cold-cache loads, Fast/Slow 4G, filmstrip, stage marks | the site or a test router | keep |
| `brotli-proxy.mjs` (64) | brotli q4 stand-in for Cloudflare in front of a test router | — | keep |
| `app-signin-check.py` (98) | iOS sign-in end to end as bot@deyaochen.com via `/api/jarvis/app-signin` (Access emailed code; ~5 codes per few minutes) | router route, email-bot | pigeon keeps `/api/jarvis/app-signin` (rename per clean cut) |
| `ios-app-record.sh` (22) | one-time ASC record + TestFlight group | apple-dev skill; **stale** `DE0CH/pwa-shell-ios` (`:20`) | fix or drop |

The browser-test runbook (`skill/SKILL.md:266-295`: build `dist/` locally ~6 GB, tar into a ui pod, run a second
router, `kubectl port-forward` through an ssh -L to the control-plane node, kill stale forwards on 18090) exists only
because signed-in pages come from per-person processes. With a stateless pigeon a build under test is just a
second pigeon Deployment/pod; the procedure must be rewritten.

## 4. Runbook procedures in `YYC/yyc-ai/README.md`

| Lines | Procedure | Status / note |
|---|---|---|
| 52-53 | Rollback of the app (previous jarvis3 commit + UI image) | valid; target: aviary pins |
| 71-72 | Check a change: `kubectl kustomize yyc-ai/deploy` (and `/sealed`) | valid |
| 74-87 | Deploy a new version (jarvis3, the fork, AMI follow-up) | valid; target rewrite |
| 89-90 | Config or system prompt edits (push; ConfigMap hash rolls pods) | valid; rolls all four Deployments |
| 97-102 | ui pin, per-person process caps (30 min idle, 4 per pod) | dies with pigeon |
| 108-111 | `/jfs` in UI pods; `users/` writable by uid 1000 | dies (FUSE→magpie for ~/.claude) |
| 114-115 | temporary "check big writes" rule | depends on gVisor patch; stays in hermit's prompt text |
| 118-123 | Offline (tofu admins policy); old chat UI (pin 2156d8a) | old UI: drop after magpie import |
| 130-147 | Sandbox hosts: AWS hosts, gateway, SSH, route, state tables, capacity | hermit |
| 157-163 | Add a read-only yyc-mcp tool; check hidden tools | tools |
| 169-173 | Block/allow a host for YYC only | egress |
| 185-198 | E2B template, switch-on | behaviour switch; hermit impl or drop |
| 204-217 | ZDR: shortlist, forced ZDR, guardrail, run `test_zdr.py` before adding a model | egress |
| 219-227 | Model and titles | fixed 2026-11-04 (the live models) |
| 228-230 | Recently deleted, purge 30 days (`jarvis-purge.ts` + router `purgeWake`, `router.mjs:665-667, 920`) | magpie (scheduled purge needs a leader lock) |
| 231-240 | Budget, reset/raise, check query, backstop | egress; "30 美元" text stale |
| 242-252 | Operator page | needs an owner in the target (section 5) |
| 254-261 | Change the staff allowlist | hermit |
| 263-295 | Secrets: reseal, table | table stale (missing 3 keys); `jarvis2` naming |

## 5. What the target design does not yet say (to decide)

1. **The operator page and its snapshot have no home.** Today web serves `/internal/ops/` and aggregates pods
   (Kubernetes API via Role `web-ops-read`), hosts, sandboxes, scaler, budget, egress errors, people, view-as
   (`jarvis3 web/ops.ts:116-201, 240-520`). In the target the data is split across hermit (sandboxes, hosts, scaler,
   people), egress (budget, spend, errors), magpie (conversations, view-as) and the cluster. Something must
   aggregate it over REST; pigeon forwards `/ops` only for admins. The news-stream dashboard depends on the same
   snapshot (2.29). The Kubernetes-API read is YYC-specific and belongs in hermit or a YYC ops piece, not in
   pigeon/magpie.
2. **Admin REST routes replace every SQL-only knob:** host cap/retire (2.7), the burst/wait test (2.8), sandboxes off
   (2.9), each with an audit row. Without them the "each owns its DB behind its API" rule is broken by the runbook.
3. **Roles in hermit's contract:** "who may use the deployment" must return admin vs staff, because pigeon (ops
   gate, view-as start), magpie (view-as enforcement) and the ops view all need it. Re-read live, not at start.
4. **Model facts come from the `model` endpoint**, not config: supervisor (Claude Code env), magpie (title model),
   pigeon (label). Otherwise model switches are deployment config in the forbidden components.
5. **Prompt content comes from hermit**, written by the supervisor the same way everywhere (2.4).
6. **Per-pair credentials with overlap for rotation**, replacing the four-job `JARVIS_CC_TOKEN` and the
   sent-files token; pigeon's view-as key is its own secret (2.11, 2.16, 2.17).
7. **Config changes should not roll everything:** today one config.json edit rolls web, egress, tools and ui. In the
   target each YYC implementation reads its own config (or the catalogue) and re-reads it live where the value is
   policy (users, budget, deny hosts).
8. **Admin read of a conversation** (2.12) and a cross-service trace by conversation id (2.13).
9. **Ported acceptance scripts** (section 3), and a test pigeon for builds under test.
10. **Cleanup to carry along:** kv leaks (`presence:*`, `replica:*`, `turn_sandbox:*`), the gateway's `hub` row in
    `vm_hosts`, 199 gone hosts, 13 legacy `ops_events` `budget` rows, the tracked `cloudflare.cpython-312.pyc`, and the
    stale docs listed above (README model/budget text and Secrets table, skill `cloudflare.py`/`UNDER_CONSTRUCTION`/
    `UI_CUTOVER`/`rollout restart`, SANDBOX-VM "force a burst", CLAUDE-CODE.md "Neon" and "USD 30", ios-app-record's
    `pwa-shell-ios`).
11. **`openrouter.py --verify` tests nothing for a non-Anthropic model** (2.3) and should test the model's own
    non-ZDR provider; the AMI workflow uses jarvis3 `main` instead of the pin (2.21).

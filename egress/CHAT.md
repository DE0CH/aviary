# Jarvis 3 chat runtime

Status: being built 2026-10-20 for the YYC instance (YYC AI, `ai.yyc.deyaochen.com`). Agreed design: Deyao,
2026-10-20. This is a second runtime next to the task runtime in `server/` (OpenCode in Fly machines): a staff web
chat whose agent loop runs **on the server**, with bash sent to a per-conversation sandbox. The code is in `chat/`,
its Kustomize base in `deploy/chat/`. An instance pins a tag of this repo and supplies its config and secrets.

Staff never see the name "Jarvis 3": the product name, logo and wording come from the instance config.

## Components (one image `ghcr.io/de0ch/jarvis3-chat`, role chosen by `JARVIS_ROLE`)

| Role | What | Replicas |
|---|---|---|
| `web` | Web UI, API, SSE streams, turn runner (spawns `claw`), sandbox manager, files, janitor | 2, rolling, drained |
| `egress` | The only way out of a sandbox; also the model gateway for claw. Token identity, per-conversation host allowlist, credential injection, logs every call, package cache | 2, rolling |
| `tools` | MCP server (Streamable HTTP, JSON responses): news search, web search/fetch, upstream MCP tools, confirmations | 2, rolling |

Plus the sandbox image `ghcr.io/de0ch/jarvis3-sandbox` (runs `sandbox-agent`): built as an E2B template for hosted
sandboxes (`sandbox.provider: "e2b"`, the YYC instance), or started as gVisor pods (`runtimeClassName: gvisor`) in
namespace `jarvis-sandbox` (`"kubernetes"`: tests, local runs, a self-hosted cluster). See "Sandbox providers".

```
browser ──Access──▶ web ──spawn per turn──▶ claw (cwd = conversation workdir, file tools confined there)
   ▲  SSE (events from Postgres)            │ model calls ─▶ egress /x/model ─▶ OpenRouter (real key injected)
   │                                        │ MCP (stdio bridge) ─▶ tools ─▶ Exa / TikHub / Wisers relay / yyc-mcp
   │                                        │ bash ─▶ jarvis-bash ─sync files + exec─▶ sandbox pod (gVisor)
   └── confirmations (outward actions) ◀────┘                                    │ all egress
                                                                                 ▼
                                                                      egress (token → conversation → allowlist)
State: Postgres (conversations, messages, events, claw session files, file metadata, sandboxes, logs); file bytes in S3.
```

### Why these choices (2026-10-20)

- **Sandbox scope = one per conversation.** The workdir belongs to a conversation, so the sandbox mirrors exactly it;
  a prompt injection in one chat can't reach another chat's files; "the sandbox was reset" means one thing.
- **Sandbox provider = a hosted sandbox service, E2B (Deyao, 2026-10-20):** fast startup (a sandbox resumes from
  the template's memory snapshot, agent already running) and no nodes to manage; "we can pay a slight premium". E2B
  passed the hard requirement: a sandbox's outbound traffic can be limited to one host (`network.allowOut` with
  `denyOut: ["0.0.0.0/0"]`), our egress proxy, so every byte still goes through `egress` with the sandbox's token.
  Its SDK, CLI and infrastructure are Apache-2.0. The gVisor-pod provider stays for tests, local runs and any
  instance that wants its own cluster. Staff files sit in E2B's sandbox only while it lives; the record stays in
  Postgres and the instance's bucket.
- **claw runs on the server, never inside a sandbox.** It holds no secrets: its model calls go to `egress`
  with a per-turn token, and its tools are MCP calls to `tools` with the same token.

## Sandbox lifecycle (design "B")

- **Warm on typing:** the UI calls `POST /api/conversations/:id/warm` (debounced, at most every 30 s) when the staff
  member types; `web` makes sure the conversation has a live sandbox (creates the pod if none). A new conversation
  warms on its first keystroke too (the UI creates the conversation row first).
- **Kept while active:** installs, caches and background processes carry over between turns.
- **Released** after `sandbox.idleMinutes` (default 30) with no turn and no warm call, and always at
  `sandbox.maxLifetimeHours` (default 8). Before deleting the pod the janitor pulls the sandbox's changed files and
  saves them (files are also synced after every bash command, so this only catches what background processes wrote).
- **Reset notice:** when a turn starts on a fresh sandbox and the conversation had an earlier one, the turn's prompt
  gets a line: `[System note: the sandbox was reset: files in the workspace are restored; installed packages and
  background processes from before are gone.]`
- **No secrets in a sandbox.** Its env has only `HTTP_PROXY`/`HTTPS_PROXY`/`http_proxy`/`https_proxy`
  `=http://sbx:<egress token>@egress.<ns>.svc.cluster.local:3128`, `NO_PROXY=` (empty), the pip/npm mirror settings
  and its exec token. NetworkPolicy: egress only to the `egress` pods (3128) and cluster DNS; ingress only from `web`
  pods on 7070.
- Limits per sandbox: `sandbox.cpu` (default "1"), `sandbox.memory` (default "1Gi"), ephemeral storage 5Gi;
  at most `sandbox.maxActive` (default 6) live sandboxes per instance; over the limit the least recently active idle
  one is released first; if none is idle, the warm/turn waits up to 60 s, then the bash tool reports
  "the sandbox pool is full, try again in a minute".

## Sandbox providers (`sandbox.provider`)

The sandbox manager (`chat/src/web/sandbox.ts`) owns the lifecycle above and the `sandboxes` rows; a provider
(`chat/src/web/provider.ts`) only creates (`create` → its name for the sandbox, stored in `sandboxes.pod`), waits for
the agent (`waitReady` → `{url, headers}`, stored in `sandboxes.agent`), keeps alive, destroys and lists. jarvis-bash
gets the URL and headers (`JARVIS_SANDBOX_URL`, `JARVIS_SANDBOX_HEADERS`), so it works the same with either.

- **`kubernetes`** (`sandbox-k8s.ts`): a gVisor pod per sandbox in `sandbox.namespace`, agent at `http://<pod IP>:7070`,
  the env (proxy, mirrors, `JARVIS_EXEC_TOKEN`) in the pod spec, the proxy `sandbox.egressProxyUrl` (default
  `http://egress.<ns>.svc.cluster.local:3128`). No provider timeout: `keepAlive` is a no-op.
- **`e2b`** (`sandbox-e2b.ts`, REST with fetch, `X-API-Key: $E2B_API_KEY`, base `https://api.e2b.app`):
  - **Template:** the chat workflow builds `chat/sandbox/Dockerfile` as E2B template `jarvis3-sandbox`, tagged with
    the commit's 12 characters, 1 vCPU / 1024 MB, start command `JARVIS_INIT=1 node /opt/agent/agent.mjs`, ready
    when `/livez` answers. The instance pins it: `sandbox.e2b.template` = `jarvis3-sandbox:<tag>`. The start command
    runs at build time and is snapshotted, so per-sandbox env can't reach it: the agent waits for `POST /init`.
  - **Create:** `POST /v2/sandboxes` `{templateID, timeout: 900, autoPause: false, metadata: {jarvis: <instance>,
    sandbox, conversation}, network: {allowPublicTraffic: false, allowOut: [<egress host>], denyOut: ["0.0.0.0/0"]}}`.
    No env vars, nothing secret. `sandboxes.pod` = E2B's `sandboxID`.
  - **Ready:** `POST https://7070-<id>.e2b.app/init` `{token: <exec token>, env: {proxy and mirror settings, all
    http://sbx:<egress token>@127.0.0.1:3128}, wire: {url: sandbox.e2b.egressUrl, headers: {Authorization: Bearer
    <egress token>, …sandbox.e2b.egressHeaders}}}`, then `/healthz`. Every call to the agent port carries
    `e2b-traffic-access-token` (from the create answer; without it E2B's edge answers 403) and the exec token in
    `X-Jarvis-Exec-Token` (a port proxy may strip `Authorization`).
  - **Egress:** the agent's local proxy `127.0.0.1:3128` turns each TCP connection into one WebSocket to egress's
    `/wire` (`wss://<egress host>/wire`, through the Cloudflare tunnel); egress feeds it into its own proxy server, so
    CONNECT, plain HTTP, the mirror, the allowlist, the address checks and the log are the same as on 3128. E2B checks
    the allow-list by TLS SNI (443) and Host (80), and lets its DNS server (8.8.8.8) through.
  - **Lifetime:** E2B kills a sandbox at its timeout. On every warm, every turn and every minute of a running turn
    (at most every 30 s per sandbox) the manager sets it (`POST /sandboxes/{id}/timeout`) to the idle release + 5
    minutes, capped at the hard maximum + 5 minutes, so the janitor's release (with the final save) always comes
    first, and a sandbox whose web replicas died still ends. Release = `DELETE /sandboxes/{id}`.
  - **Janitor:** `GET /v2/sandboxes?metadata=jarvis=<instance>`; a sandbox older than a minute with no live row is
    killed. Other instances' sandboxes (another `jarvis` tag) are never touched.
  - **Plan limits:** E2B Hobby ends any sandbox after 1 hour of continuous running; `sandbox.maxLifetimeHours` above 1
    needs Pro (24 hours).
  - **Measured** (2026-10-20, us-west-1, `chat/scripts/e2b-live.ts`, which re-runs the whole check: create, `/init`,
    jarvis-bash, `pip install` through the mirror, allowed and refused hosts, direct internet blocked by name and by
    IP, kill): create 1.1 s, `/init` + health 0.6 s; a pip install of a small package 6.4 s. Cloudflare refuses a
    request whose Host differs from the egress host's SNI (403), so the SNI allow-list can't be used to reach other
    sites behind Cloudflare.

## Interfaces

### Postgres schema (owned by `web`, migrations in `chat/src/common/schema.ts`, applied under an advisory lock)

```sql
create table users (email text primary key, name text, is_admin boolean not null default false,
  created_at timestamptz not null default now(), last_seen_at timestamptz);
create table conversations (id uuid primary key, owner text not null references users(email),
  title text not null default '', model text not null, created_at timestamptz not null default now(),
  updated_at timestamptz not null default now(), deleted_at timestamptz);
create table messages (id bigserial primary key, conversation_id uuid not null references conversations(id),
  role text not null check (role in ('user','assistant')), content text not null,
  turn_id uuid, attachments jsonb not null default '[]', created_at timestamptz not null default now());
create table turns (id uuid primary key, conversation_id uuid not null references conversations(id),
  status text not null check (status in ('queued','running','waiting','done','error','cancelled')),
  token_hash text not null unique,            -- sha256 hex of the per-turn token (egress /x/model + tools MCP)
  replica text, model text not null, started_at timestamptz not null default now(), finished_at timestamptz,
  error text, usage jsonb, cost_usd numeric);
create table events (id bigserial primary key, conversation_id uuid not null, turn_id uuid not null,
  seq int not null, type text not null, data jsonb not null, created_at timestamptz not null default now(),
  unique (turn_id, seq));
create table claw_sessions (conversation_id uuid primary key, content text not null, updated_at timestamptz not null default now());
create table files (conversation_id uuid not null, path text not null, sha256 text not null, size bigint not null,
  source text not null check (source in ('upload','agent')), updated_at timestamptz not null default now(),
  deleted boolean not null default false, primary key (conversation_id, path));
create table sandboxes (id uuid primary key, conversation_id uuid not null, pod text not null unique,
  egress_token_hash text not null unique, exec_token text not null,   -- exec token: web<->agent only, never in Postgres logs
  status text not null check (status in ('starting','ready','releasing','gone')),
  created_at timestamptz not null default now(), last_active_at timestamptz not null default now(),
  expires_at timestamptz not null, released_at timestamptz, release_reason text);
create table allowed_hosts (conversation_id uuid not null, host text not null, added_by text not null,
  created_at timestamptz not null default now(), primary key (conversation_id, host));
create table egress_log (id bigserial primary key, at timestamptz not null default now(), conversation_id uuid,
  sandbox_id uuid, turn_id uuid, kind text not null,   -- 'connect' | 'http' | 'mirror' | 'model' | 'inject'
  method text, host text not null, port int, path text, status int, decision text not null,  -- 'allow' | 'deny:<why>'
  bytes_up bigint, bytes_down bigint, ms int, cost_usd numeric);
create table tool_calls (id bigserial primary key, at timestamptz not null default now(), turn_id uuid,
  conversation_id uuid, user_email text, tool text not null, args jsonb, ok boolean, ms int, error text, cost_usd numeric);
create table confirmations (id uuid primary key, conversation_id uuid not null, turn_id uuid not null,
  kind text not null, summary text not null, detail jsonb not null default '{}',
  status text not null check (status in ('pending','approved','denied','expired')),
  created_at timestamptz not null default now(), decided_at timestamptz, decided_by text);
create table kv (key text primary key, value jsonb not null);
```

Notifications: `pg_notify('conv_<uuid without dashes>', '<event id>')` after inserting events, and
`pg_notify('confirm_<confirmation uuid without dashes>', status)` when a confirmation is decided.

### Tokens

- **Turn token** (32 random bytes, base64url), made by `web` per turn, stored as `turns.token_hash`, valid while the
  turn is `running`/`waiting`. claw gets it as `ANTHROPIC_AUTH_TOKEN` (egress `/x/model`) and the MCP bridge sends it
  as `Authorization: Bearer` to `tools`.
- **Egress token**, per sandbox, stored as `sandboxes.egress_token_hash`; valid while the sandbox is not `gone`.
- **Exec token**, per sandbox: `web` → `sandbox-agent` only (`Authorization: Bearer`).

### `egress` (port 3128, plus 9090 for /healthz and /metrics)

1. **Forward proxy for sandboxes.** `CONNECT host:port` and absolute-URI HTTP requests with
   `Proxy-Authorization: Basic base64("sbx:<egress token>")`. Unknown/expired token → 407. Allowed when the host
   matches `egress.defaultAllowedHosts` (config; suffix match on entries starting with `.`) or the conversation's
   `allowed_hosts`. Ports: 80, 443 only. Everything else → 403 with a plain-text body naming the host and saying the
   agent can ask the user to allow it (`request_network_access` tool). Every request → `egress_log`.
   Private, loopback, link-local and cluster addresses are refused after DNS resolution (no SSRF into the cluster).
   **The wire:** `GET /wire` with a WebSocket upgrade and `Authorization: Bearer <egress token>` (401 otherwise):
   the WebSocket's binary messages are one proxy connection, handed to this same server, so everything above
   applies unchanged. Pings every 30 s. This is how hosted sandboxes (E2B) reach egress through the tunnel.
2. **Package mirror** (cache on the pod's volume, LRU at `egress.cacheGiB`, default 5):
   `GET /mirror/pypi/simple/...` (from pypi.org, links rewritten to `/mirror/pypi/files/...` = files.pythonhosted.org),
   `GET /mirror/npm/...` (registry.npmjs.org, tarball URLs rewritten), apt via the forward proxy: plain-HTTP GETs to
   `deb.debian.org`/`security.debian.org` are cached too. Immutable artefacts cached; index pages for 5 minutes.
   Auth: same Basic token (pip/npm accept `http://sbx:<token>@host/...`), in `Authorization` or
   `Proxy-Authorization`. A mirror or route request may also arrive in proxy form (`GET http://egress…/mirror/…`, what
   pip/npm send with `HTTP_PROXY` set and an empty `NO_PROXY`): egress answers it itself when the host is one of its
   own names (`EGRESS_SELF_HOSTS`, default `egress`, `egress.<POD_NAMESPACE>`, `….svc`, `….svc.cluster.local`; plus
   always `127.0.0.1` and `localhost`, a hosted sandbox's end of the wire).
   npm tarball URLs are rewritten to `http://<the request's Host>/mirror/npm/…`, pypi file links to the root-relative
   `/mirror/pypi/files/…`.
3. **Credential-injecting routes** `ANY /x/<route>/<path>`: routes from config `egress.routes`
   (`{name, upstream, inject: {header: value-from-env-var}, auth: "turn"|"sandbox", allowPaths?}`). The YYC instance
   has one: `model` → `https://openrouter.ai/api`, auth `turn` (`Authorization: Bearer <turn token>` or
   `x-api-key`), injects `Authorization: Bearer $OPENROUTER_API_KEY`, forces zero data retention into every request
   body (`provider.zdr: true`, `provider.data_collection: "deny"`, set over whatever the caller sent; a body that is
   not a JSON object is refused with 400, `deny:body`; no config turns this off: `forceZdr` in
   `chat/src/egress/proxy.ts`), streams responses through, logs `cost_usd` from
   the response's `usage.cost` (SSE: the final `message_delta` usage) into `egress_log` (kind `model`).
   The caller's `Authorization`, `x-api-key` and `Cookie` never go upstream; egress asks upstream for an
   uncompressed body (`accept-encoding: identity`) so it can read the cost.
4. **Env:** `DATABASE_URL`, `JARVIS_CONFIG`, `JARVIS_CACHE_DIR` (default `/var/cache/jarvis-egress`, an emptyDir),
   `EGRESS_SELF_HOSTS`, `POD_NAMESPACE`, `EGRESS_BLOCK_CIDRS` (extra refused ranges, comma list: the node's own
   public /64 on YYC), `EGRESS_DRAIN_SECONDS` (default 120), and every variable a route's `inject` names
   (`OPENROUTER_API_KEY`). Identity lookups are cached for 5 s, a conversation's `allowed_hosts` for 2 s.
   On SIGTERM: `/healthz` turns 503, the listener closes, open requests and tunnels get up to the drain time,
   the log is flushed.

### `tools` (port 8080, `POST /mcp`, MCP Streamable HTTP, stateless, JSON responses)

- Auth: `Authorization: Bearer <turn token>` → turn → conversation → owner email. Invalid → 401.
- Every call → `tool_calls`.
- Tools (names lowercase, `snake_case`; annotations set `readOnlyHint` correctly):
  `web_search`, `web_fetch`, `news_search` (Wisers), `social_search` (TikHub), plus every tool of each configured
  upstream MCP server, exposed as `<prefix>_<tool>` (YYC: prefix `yyc`, the yyc-mcp Worker, service token +
  `X-YYC-User: <owner email>`), and `request_network_access(host, reason)`.
- **Outward actions need the staff member's confirmation:** a tool marked `confirm` in config (and
  `request_network_access` always) inserts a `confirmations` row, emits an event through `web`
  (`POST http://web.<ns>.svc:8080/internal/events` with the turn token: `{type:"confirmation_request", id, kind,
  summary, detail}`), then waits (LISTEN `confirm_<id>`, up to 15 minutes) for approve/deny. Denied/expired → the
  tool returns an error text "The user declined." / "No answer from the user." On expiry `tools` itself sets the row
  `expired` and posts `{type:"confirmation_resolved", id, status:"expired"}`; on approve/deny `web` posts it.
  `kind` is `network_access` (detail `{host, reason}`) or `tool` (detail `{tool, arguments}`).
- A built-in tool whose provider is not configured is left out of `tools/list` (`web_search` needs `tools.exa`,
  `news_search` `tools.wisers`, `social_search` `tools.tikhub`; `web_fetch` without Exa reads pages directly).
  `web_fetch`'s direct path refuses non-public addresses (checked after DNS, connecting to the checked address) and
  ports other than 80/443/8080/8443. `tools.tikhub.platforms` (optional) limits `social_search` to a subset of
  `xiaohongshu, douyin, weibo, x, threads, youtube, instagram, reddit`. Upstream tool lists are read at startup and
  every 10 minutes as the first admin (`${USER_EMAIL}` = that admin while listing, the turn's owner on calls).
- Env: `PORT` (8080), `JARVIS_WEB_URL` (default `http://web.$POD_NAMESPACE.svc.cluster.local:8080`),
  `JARVIS_CONFIRM_TIMEOUT_MS` (15 min), `JARVIS_DRAIN_MS` (16 min: a held confirmation finishes before the pod
  stops; `terminationGracePeriodSeconds` ≥ 1020), plus `DATABASE_URL` and the key env vars the config names.
- **`jarvis-mcp-bridge`** (claw's stdio MCP server `tools`, in the image at `src/bin/`): reads JSON-RPC framed
  with `Content-Length` headers (claw's MCP client) or one message per line, answers in the same framing, POSTs each
  message to `$JARVIS_TOOLS_URL` (`http://tools.<ns>.svc.cluster.local:8080/mcp`) with
  `Authorization: Bearer $JARVIS_TURN_TOKEN`; retries a 503 (a draining replica) up to 3 times.

### `web` internal endpoints (cluster-only; NetworkPolicy admits `tools` pods)

- `POST /internal/events` — `Authorization: Bearer <turn token>`, body = one event object; appended to the turn's
  events (same as claw's own events).

### `sandbox-agent` (port 7070, in the sandbox pod, `Authorization: Bearer <exec token>`)

The workspace is `/work` (owned by uid 1000, the agent and commands run as uid 1000; `$HOME=/home/sandbox`).

- `GET /healthz` → 200 `{ok:true, boot_id}` (no token needed: the kubelet's readiness probe); 503 while it waits for
  `/init`. A new `boot_id` means the agent restarted and every earlier cursor is void. `GET /livez` → 200 always.
- Auth: the exec token in `X-Jarvis-Exec-Token` (what the web role sends) or `Authorization: Bearer`.
- `POST /init` (only with `JARVIS_INIT=1` and no `JARVIS_EXEC_TOKEN`, once, no token): `{token, env, wire?: {url,
  headers}}` sets the exec token, adds `env` to every command's environment and, with `wire`, starts the local proxy
  on `127.0.0.1:3128` (`JARVIS_WIRE_PORT`) that carries each connection to egress's `/wire` over a WebSocket. A
  second call answers 409.
- `PUT /files` — body: a tar stream (uncompressed) of files to write under `/work`; header `X-Delete: <JSON array of
  relative paths>` for deletions. Returns `{ok, written, deleted}`.
- `POST /exec` — `{command, timeout_ms (default 120000, max 600000), background: bool}` → runs `bash -lc` in `/work`.
  Foreground: `{stdout, stderr, exit_code, interrupted, ms}` (each stream capped at 1 MB, tail kept, `truncated`
  flag). Background: `{background_id, pid, log}` with output going to `log` = `.jobs/<id>.log` (under `/work`, so
  it syncs back like any file). The command's process group is killed on timeout (`interrupted: true`); a process a
  foreground command leaves running is not killed, but its output after the command ends is not collected.
- `GET /changes?since=<cursor>` → tar stream of regular files under `/work` modified since the cursor (excluding
  `egress.syncExclude`-style patterns: `node_modules/`, `.venv/`, `venv/`, `__pycache__/`, `.cache/`, `.git/objects/`,
  files > 50 MB), header `X-Cursor: <new cursor>`, `X-Deleted: <JSON array>` (paths gone since the cursor),
  `X-Skipped: <JSON array>` (changed files over 50 MB, not sent), `X-Full: 1|0`.
  `since` empty or unknown (e.g. after an agent restart) = everything, `X-Full: 1`, no deletions (the caller must
  treat a full answer as the complete list). When the two lists don't fit in 8 KB of headers they move into the
  tar's first entry `.jarvis-meta/changes.json` = `{deleted, skipped}` and the response has
  `X-Meta-Entry: .jarvis-meta/changes.json` instead of `X-Deleted`/`X-Skipped`. Files the caller wrote or deleted
  with `PUT /files` are not reported back. Symlinks are never followed or sent.
- `PUT /files` refuses (400) absolute paths, `..`, and any path whose real parent directory is outside `/work`
  (a symlink planted by a command). Tar entries other than regular files are skipped.
- Running processes survive between calls (tini is PID 1 and reaps; the agent's env loses `JARVIS_EXEC_TOKEN`
  before any command runs).
- Tar on both sides: `chat/sandbox/tar.mjs` (`packBuffers`, `entryHeader`/`entryPadding`/`END` to stream,
  `unpack`; ustar + PAX long names; types in `tar.d.mts`). The `web` image can import it for `jarvis-bash`.

### `jarvis-bash` (claw's bash, `CLAW_BASH_COMMAND`)

claw (fork patch) runs `$CLAW_BASH_COMMAND` with the bash tool input JSON on stdin
(`{command, timeout, description, run_in_background}`) and cwd = workdir, and expects JSON on stdout:
`{stdout, stderr, exit_code, interrupted, background_task_id?}` (non-zero exit of the helper = tool error with its
stderr). The helper (in the `web` image) gets `JARVIS_SANDBOX_URL`, `JARVIS_SANDBOX_TOKEN`, `JARVIS_SYNC_STATE`
(a JSON file outside the workdir with the last-synced manifest and the `/changes` cursor) from the turn runner. Each
call: push files that changed in the workdir since the manifest (`PUT /files`), `POST /exec`, then pull
`/changes` into the workdir and update the manifest. It also needs `JARVIS_SANDBOX_ID`: a different id than the one in
the state file (a new sandbox) resets the manifest and cursor, so the whole workdir is pushed again. A full `/changes`
answer (`X-Full: 1`) is the complete list: files the manifest has and the answer lacks (and didn't skip) are deleted
locally. Background commands answer `Started in the background … output goes to .jobs/<id>.log`; `.jobs/` is pulled
into the workdir (the agent can read it) but never stored in the bucket. Code: `chat/src/web/bash.ts`.

### claw fork patches (DE0CH/claw-code, all no-ops unless their env var is set)

| Env | Effect |
|---|---|
| `CLAW_SYSTEM_PROMPT_FILE` | the system prompt = that file's text plus an `# Environment` section (real date, cwd); no coding sections, no git context, no instruction-file discovery, no runtime-config dump |
| `CLAW_BASH_COMMAND` | the `bash` tool runs that executable as above instead of a local shell |
| `CLAW_CONFIG_DIR` | the only config dir read (settings, `mcpServers`); nothing is read from the workdir |
| `CLAW_SESSIONS_DIR` | where managed sessions live (instead of `<cwd>/.claw/sessions/…`), no workspace-root check |
| `CLAW_PROMPT_CACHE=1` | `cache_control: ephemeral` on the system prompt, the tool list and the last user message (Anthropic format) |

### Turn runner (in `web`)

1. `POST /api/conversations/:id/messages` → insert the user message and a `turns` row (`queued`), answer 202 with ids.
   One running turn per conversation (409 otherwise).
2. The receiving replica runs it: reconcile the local workdir `/var/lib/jarvis/work/<conv>` with `files` (download
   from S3 what differs), write `claw_sessions.content` to `<sessions dir>/<conv>.jsonl`, ensure the sandbox, then
   spawn `claw --model <model> --permission-mode danger-full-access --allowedTools <list> --output-format json
   prompt <text>` with `CLAW_EVENT_STREAM=1`, `CLAW_RESUME_SESSION=<path>` (when a session exists), and the env above.
   The MCP config (in `CLAW_CONFIG_DIR/settings.json`) lists one stdio server `tools` = `jarvis-mcp-bridge` (stdio ↔
   HTTP, token from env).
3. Each NDJSON event → `events` (+ `pg_notify`). Text deltas are batched (≤ 250 ms) into one `text_delta` event.
4. End: save the session file to `claw_sessions`, upload changed workdir files to S3 (`files/<conv>/<sha256>`) and
   upsert `files` (source `agent`), insert the assistant message (final text), set the turn `done` with usage and
   cost. Title: after the first turn, a short title from the model (Haiku-class, via egress) if the user did not set one.
5. Cancel: `POST /api/turns/:id/cancel` → SIGTERM to claw (then SIGKILL after 5 s) on whichever replica runs it
   (each replica LISTENs on `cancel_<replica>`).
6. **Drain:** on SIGTERM a replica stops being ready (readiness 503), refuses new turns (another replica takes them:
   the UI retries 503), and waits for its running turns up to `terminationGracePeriodSeconds - 30` (manifests: 1800 s).
7. **Reset notice:** kv `turn_sandbox:<conv>` holds the sandbox id the conversation's last turn used; a turn on a
   different sandbox gets the reset line prepended to its prompt and a `sandbox {status: "reset"}` event.
8. A turn whose sandbox can't be had (pool full past 60 s, start failure) still runs; its `sandbox {status:
   "unavailable", message}` event says why and `jarvis-bash` then fails with that reason. claw's session file is saved
   only after a `done` turn (a cancelled or failed turn may leave a tool call without its result, which the model API
   refuses on replay); the turn's files are stored whatever its outcome.
9. Replicas write a heartbeat (kv `replica:<name>`, every 10 s); the janitor (one replica, advisory lock, every 30 s)
   ends turns of replicas silent for 60 s and queued turns older than 2 minutes (`error`, "interrupted: the server
   restarted"), expires confirmations after 15 minutes, releases idle/expired sandboxes, ends stuck starts, deletes
   sandbox pods without a live row and marks rows whose pod vanished.

### Events (what the UI renders; `GET …/stream` and `turns[].events`)

Each is `{id, turnId, seq, type, data, createdAt}`; the list of types is in "Public API" below. What `web` itself adds:
`turn_started {turnId, model}`; `sandbox {status: starting|ready|reset|unavailable, message?}`; `files {changed:
[path]}` (what the turn stored); `title {title}`; `confirmation_resolved {id, status}` on approve/deny (and, as a
backstop for a tools pod that died, on expiry after 20 minutes; `tools` posts the expiry itself at 15);
`turn_finished {turnId, status: done|error|cancelled, error, costUsd}`. claw's `result` is stored as it comes
(`usage`, `estimated_cost`, `message`, …); its `session` line is not stored (a server path). `tool_result.output` is
capped at 20 000 characters (`truncated: true`). The fake claw in `chat/test/fakes/claw.mjs` also emits a `debug`
event (tests only).

### `web` deployment needs

- Env: `JARVIS_ROLE=web`, `DATABASE_URL`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `JARVIS_NAMESPACE` (its own
  namespace, from the downward API; default `jarvis`), `HOSTNAME` (the pod name: the replica's name), optional
  `JARVIS_EGRESS_URL`, `JARVIS_TOOLS_URL`, `JARVIS_WORK_ROOT` (default `/var/lib/jarvis/work`, an emptyDir),
  `JARVIS_DRAIN_SECONDS` (default 1770), `JARVIS_UI_DIR`, `JARVIS_BRAND_DIR` (default `/etc/jarvis/brand`, served at
  `/brand/*`), `CLAW_PATH`. Config at `/etc/jarvis/config.json`, the system prompt file it names.
- RBAC (provider `kubernetes` only): a ServiceAccount (token mounted) bound to a Role in the sandbox namespace with
  `pods`: create, get, list, watch, delete and `pods/status`: get. Nothing else.
- Provider `e2b`: `E2B_API_KEY` (from `web-env`) and any variable `sandbox.e2b.egressHeaders` names; outbound HTTPS to
  `api.e2b.app` and `*.e2b.app`.
- Pod: `terminationGracePeriodSeconds: 1800`, readiness `GET /healthz`, a writable emptyDir at the work root; the image
  runs as uid 1000. NetworkPolicy: ingress from cloudflared and the `tools` pods (`/internal/events`); egress to
  Postgres, the bucket, the Kubernetes API, `egress` and `tools` Services and sandbox pods on 7070.

### Public API (`web`, all behind Access; JSON)

- `GET /api/me` → `{email, name, isAdmin, product: {name, logoUrl, locale}, models: [{id, label}], defaultModel}`
- `GET /api/conversations` → newest first `[{id, title, model, updatedAt, running}]`; conversations without any
  message are left out (the UI creates a conversation on the first keystroke of a new chat, to warm its sandbox)
- `POST /api/conversations` `{model?}` → `{id}`; `PATCH /api/conversations/:id` `{title?, model?}` → 204 (the
  model applies from the next turn); `DELETE` (soft) → 204
- `GET /api/conversations/:id` → `{id, title, model, messages: [{id, role, content, turnId, attachments: [path],
  createdAt}], turns: [{id, status, model, startedAt, finishedAt, error, costUsd, events: [event]}],
  pendingConfirmations: [{id, turnId, kind, summary, detail, status}], lastEventId}` (oldest first; stream from
  `lastEventId` to continue)
- `POST /api/conversations/:id/warm` → 204
- `POST /api/conversations/:id/messages` `{text, attachments: [path]}` → 202 `{turnId}`; 409 while a turn is going;
  503 while this replica drains (retry)
- `GET /api/conversations/:id/stream?after=<event id>` → SSE of events with an id greater than `after` (or the
  `Last-Event-ID` header, whichever is greater), then live ones; `id:` = event id, `data:` = one event as JSON
  `{id, turnId, seq, type, data, createdAt}`; heartbeats (`: hb`) every 15 s
- Event types: claw's (`session`, `message_start`, `text_delta{text}`, `thinking_delta`, `tool_use{id,name,input}`,
  `tool_result{id,name,output,is_error}`, `message_stop`, `result{usage, estimated_cost, …}`, `error{message}`) and
  the server's: `turn_started{turnId}`, `turn_finished{turnId, status, error?}`, `confirmation_request{id, kind,
  summary, detail}` (kind `network_access` has `detail: {host, reason}`), `confirmation_resolved{id, status}`,
  `sandbox{status: starting|ready|reset|unavailable}`, `title{title}`, `files{changed}`. MCP tools are named `mcp__tools__<tool>`.
- `POST /api/turns/:id/cancel` → 204
- `POST /api/confirmations/:id` `{approve: bool}` → 204
- `POST /api/conversations/:id/files?name=<file name>` (raw body, `content-type` = the file's, ≤ 100 MB) → 201
  `{path, size}` (under `uploads/`, ` (2)` added before the extension when the name is taken)
- `GET /api/conversations/:id/files` → `[{path, size, source: "upload"|"agent", updatedAt}]`;
  `GET /api/conversations/:id/files/<path>` (each segment URL-encoded) → the bytes, `content-disposition` with the
  file name
- Errors: JSON `{error: "<message>"}` with the status; 401 = no valid Access token, 403 = signed in but not on
  `config.users` (the UI shows "not on the list"); 503 while draining (the UI retries)
- Admin only: `GET /api/admin/usage?from&to` (per user: turns, model cost, tool calls, egress), `GET /api/admin/egress`
- Static: the UI build (`chat/ui-dist`, `npm run build` in `chat/ui`) at `/` with the SPA fallback for `/c/<id>`
  (`index.html` never cached, hashed `app-*.js|css` immutable); `/brand/logo.svg` and `/brand/logo.png` from the
  instance (the UI falls back to the product name's initials); `/manifest.webmanifest` with `name`/`short_name`
  replaced by the product name.
- `GET /healthz` (no auth; readiness = not draining and Postgres reachable)

Authorisation: Access JWT (team domain + audience from config) → email → must be in `config.users`
(the instance's allowlist, the same list as the Access policy) → a `users` row is upserted. A user sees only their
own conversations (others' ids answer 404); admins (`config.admins`) can read the usage/egress views, not others'
chats. A signed-in address that is not on the list gets a plain 403 page naming it (JSON 403 under `/api/`). Writes
with an `Origin` other than `publicUrl` are refused.

## Deploying an instance

The base is `deploy/chat/` (Kustomize): namespace `jarvis` (Pod Security restricted) with `web`, `egress`, `tools`
(2 replicas each, rolling with `maxUnavailable: 0`, PDBs, drained on SIGTERM: grace 1800 / 150 / 1020 s), the tunnel
connector `cloudflared`, NetworkPolicies (ingress: web ← cloudflared + tools, tools ← web, egress:3128 ← web +
cloudflared (the wire); egress: DNS, the namespace's pods, public internet only) and a LimitRange. With provider
`e2b`, sandboxes reach egress at a public hostname of the instance's tunnel routed to `http://egress.jarvis.svc:3128`
(`/wire` only is used; each request inside still needs a live sandbox's token), and `web` needs `E2B_API_KEY` in
`web-env`. With provider `kubernetes`, sandbox pods run in a **separate cluster** (namespace `jarvis-sandbox` there,
with its RuntimeClass, quota and RBAC); `web` gets its API server, CA and token from Secret `sandbox-cluster` (keys
`server`, `ca.crt`, `token`, mounted at `$JARVIS_SANDBOX_CLUSTER_DIR`), and that link also carries the sandboxes' path
to `egress:3128`. The image is the placeholder `jarvis3-chat`.

An instance is a Kustomization (YYC: `DE0CH/YYC` `yyc-ai/deploy/`) that
- uses `github.com/DE0CH/jarvis3//deploy/chat?ref=<commit>` as its base and maps `jarvis3-chat` to
  `ghcr.io/de0ch/jarvis3-chat:<sha12>` with `images:` (the sandbox image tag goes in `config.json`);
- generates ConfigMaps `jarvis-config` (`config.json`, `system-prompt.md`) and `jarvis-brand` (`logo.svg`/`logo.png`);
- supplies SOPS-encrypted Secrets `web-env` (`DATABASE_URL`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`), `egress-env`
  (`DATABASE_URL`, `OPENROUTER_API_KEY`), `tools-env` (`DATABASE_URL` + the tool keys its config names), `tunnel`
  (`token`) and `sandbox-cluster`, plus a `ghcr-pull` docker-registry Secret patched onto the Deployments while the
  GHCR packages are private;
- defines the PriorityClass `serving` the base's pods use (or patches `priorityClassName`).

A new version = bump the base `ref` and the image tags (chat image in `images:`, the sandbox's in `config.json`:
`sandbox.e2b.template` = `jarvis3-sandbox:<sha12>`, or `sandbox.image`) to a commit whose `chat` workflow pushed
images and built the template, and push; Flux rolls the Deployments one pod at a time.

## Config (`/etc/jarvis/config.json`, from the instance)

```json
{
  "product": {"name": "YYC AI", "locale": "zh-HK", "logo": "/brand/logo.svg"},
  "publicUrl": "https://ai.yyc.deyaochen.com",
  "access": {"teamDomain": "de0ch.cloudflareaccess.com", "audience": "<aud tag>"},
  "users": ["chendeyao000@gmail.com"], "admins": ["chendeyao000@gmail.com"],
  "models": [{"id": "anthropic/claude-opus-5.5", "label": "Opus"}, {"id": "anthropic/claude-sonnet-5.5", "label": "Sonnet"}],
  "defaultModel": "anthropic/claude-opus-5.5", "titleModel": "anthropic/claude-haiku-5.5",
  "systemPromptFile": "/etc/jarvis/system-prompt.md",
  "sandbox": {"provider": "e2b", "idleMinutes": 30, "maxLifetimeHours": 8, "maxActive": 6,
              "e2b": {"template": "jarvis3-sandbox:<tag>", "egressUrl": "wss://egress.ai.yyc.deyaochen.com/wire",
                      "egressHeaders": {}, "apiKeyEnv": "E2B_API_KEY", "instance": "ai.yyc.deyaochen.com"}},
  // provider "kubernetes" instead: {"provider": "kubernetes", "image": "ghcr.io/de0ch/jarvis3-sandbox:<tag>",
  //   "namespace": "jarvis-sandbox", "runtimeClass": "gvisor", "cpu": "1", "memory": "1Gi", "egressProxyUrl"?: …}
  "egress": {"defaultAllowedHosts": ["pypi.org", "files.pythonhosted.org", "registry.npmjs.org", "deb.debian.org",
             "security.debian.org", "github.com", "objects.githubusercontent.com", "raw.githubusercontent.com"],
             "routes": [{"name": "model", "upstream": "https://openrouter.ai/api", "auth": "turn",
                         "inject": {"Authorization": "Bearer ${OPENROUTER_API_KEY}"}}], "cacheGiB": 5},
  "tools": {"exa": {"keyEnv": "EXA_API_KEY"}, "tikhub": {"keyEnv": "TIKHUB_API", "platforms": ["xiaohongshu", "douyin", "weibo", "x", "threads", "youtube", "instagram", "reddit"]},
            "wisers": {"url": "https://yyc.deyaochen.com/app/wisers/api", "accessClientIdEnv": "WISERS_ACCESS_CLIENT_ID", "accessClientSecretEnv": "WISERS_ACCESS_CLIENT_SECRET"},
            "upstreams": [{"prefix": "yyc", "url": "https://yyc-mcp.yyc-news.workers.dev/mcp",
                           "headers": {"Authorization": "Bearer ${YYC_MCP_TOKEN}", "X-YYC-User": "${USER_EMAIL}"}}],
            "confirm": []},
  "s3": {"endpoint": "...", "region": "...", "bucket": "...", "prefix": "jarvis/"}
}
```

Secrets come from the environment (`DATABASE_URL`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `OPENROUTER_API_KEY`
(egress only), tool keys (tools only)). Each role gets only the Secret it needs.

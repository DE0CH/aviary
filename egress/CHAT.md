# Jarvis 3 chat runtime

The server side of a staff AI chat instance (YYC AI, `ai.yyc.deyaochen.com`): **Claude Code runs in a sandbox per
conversation**, and the staff UI is a fork of CloudCLI (DE0CH/claudecodeui) that the instance deploys itself. The
design and why: docs/CLAUDE-CODE.md. This runtime provides the sandboxes and everything between them and the outside:
the code is in `chat/`, its Kustomize base in `deploy/chat/`. An instance pins a commit of this repo and supplies its
config and secrets. It is a second runtime next to the task runtime in `server/` (OpenCode in Fly machines).

**What it is (Deyao, 2026-10-21): a read-only data-analysis tool, not an assistant that acts.** It searches, reads
and analyses (news, social posts, the instance's own data, uploaded files, a sandbox for code and data work); it never
sends, posts or changes anything outside. So nobody is ever asked anything: every tool is read-only (outward actions
such as sending mail are not offered at all), and the sandbox's internet access is decided automatically by an
allowlist with no manual override.

## Components (one image `ghcr.io/de0ch/jarvis3-chat`, role chosen by `JARVIS_ROLE`)

| Role | What | Replicas |
|---|---|---|
| `web` | `/internal/cc/*` for the UI pods: the sandbox manager, its janitor and (provider `vm`) the scaler. No public route | 2, rolling, drained |
| `egress` | The only way out of a sandbox: token identity, the automatic sandbox allowlist with threat-intel DNS, package mirror and cache, credential-injecting routes (the model with forced ZDR and the budget; our tools), logs every call | 2, rolling |
| `tools` | MCP server (Streamable HTTP, JSON responses): news search, web search/fetch, social search, upstream MCP tools; read-only tools only | 2, rolling |

Plus the sandbox image `ghcr.io/de0ch/jarvis3-sandbox` (Claude Code, Python/Node tools, Docker, the sandbox agent),
run by our own sandbox hosts (`sandbox.provider: "vm"`: gVisor, AWS burst hosts behind a gateway on a cluster node,
live migration; docs/SANDBOX-VM.md). See "Sandbox providers" for the others.

```
browser ─Access─▶ UI pod (router → one CloudCLI server per person; DE0CH/claudecodeui)
                    │ POST /internal/cc/sandbox {owner, conversation} ─▶ web ─▶ sandbox manager ─▶ provider (vm: sandboxd)
                    │ GET /proc (upgrade, exec token) ─▶ sandbox agent :7070 ─▶ `claude` (cwd /workspace)
                    ▼                                                              │ all traffic (HTTP(S)_PROXY)
                  /jfs (JuiceFS: ws/<conversation>, users/<key>/claude)            ▼
                                     egress :3128 ── sandbox allowlist → threat DNS → public internet
                                                 ├─ /x/claude ─▶ OpenRouter (key injected, ZDR forced, budget)
                                                 └─ /x/tools/mcp ─▶ tools ─▶ Exa / TikHub / Wisers relay / yyc-mcp
State: Postgres (sandboxes, vm hosts and events, egress_log, tool_calls, kv); /workspace and ~/.claude on JuiceFS.
```

## Control plane (`web`, `/internal/cc/*`)

Cluster-only (the instance's NetworkPolicy admits its UI pods to web:8080) and authenticated by the shared bearer token
`JARVIS_CC_TOKEN` (401 otherwise); POST with a JSON body; `conversation` must be a uuid (400; optional only on `warm`).

- `POST /internal/cc/sandbox {owner, conversation}` → `{id, url, token, headers, reset}`: the conversation's live
  sandbox, made if there is none, for `owner` (403 unless in config `users`). `url` + `headers` reach its agent,
  `token` is the agent's exec token. `reset: true` when the conversation had a different sandbox before (kv
  `cc_sandbox:<conversation>` remembers the last id): its installs and processes are gone, `/workspace` is not.
  Concurrent calls in one replica share one start; a start takes up to 3 minutes (a host booting).
- `POST /internal/cc/touch {conversation}`: it is in use; its idle time starts again. The UI touches every minute
  while Claude Code runs in it.
- `POST /internal/cc/release {conversation}`: the conversation was deleted; its sandbox goes at once.
- `POST /internal/cc/warm {owner, conversation?}` → `{ok, hinted}`: `owner` is typing (`conversation` absent for a
  new chat; 403 unless in `users`). Nothing is made and no idle time is reset; when the conversation has no live
  sandbox it records kv `sandbox_demand:<owner>`, which the vm scaler counts for 2 minutes and starts an AWS host
  for when no host has a free slot (docs/CLAUDE-CODE.md, "Warming").
- `GET /healthz` → `{ok, draining}` (503 while draining or without Postgres): the readiness and liveness probe.
- **Drain** (SIGTERM): readiness 503, 5 s for the Service to drop the endpoint, then the calls in flight are answered
  (up to `JARVIS_DRAIN_SECONDS`, 300, < the 330 s grace period); the janitor and scaler locks pass to the other
  replica.

## Operator page (`web`, `/internal/ops/*`; `chat/src/web/ops.ts`, `ops-page.ts`)

A read-only page for config `admins` (English: it is for the operator), at `<publicUrl>/ops/`. The UI's router
(DE0CH/claudecodeui `jarvis/router.mjs`) verifies the Access token as for any request, then forwards `GET /ops/*` to
web's `/internal/ops/*` with the bearer token and `X-Jarvis-User` only when that email is in `admins` (anyone else:
403, nothing forwarded; no CloudCLI process is involved). Web checks both again (401 without the token, 403 unless
`X-Jarvis-User` is an admin, 405 for anything but GET/HEAD). So access = Cloudflare Access (the staff app) + the
router's `admins` check + web's own check, and web stays cluster-only.

- `GET /internal/ops/` the page (self-contained HTML, strict CSP; polls `snapshot` every 10 s while visible; stacked
  cards on a phone, columns on a wide screen).
- `GET /internal/ops/snapshot?tz=<IANA zone>` → JSON (cached 4 s per zone; "today"/"this month" start at that zone's
  midnight, default Asia/Hong_Kong). Each section is computed on its own (8 s timeout); a failed one is `null` with its
  message in `errors.<section>`:
  - `sandboxes`: live rows (conversation, owner, host, status, age, last active, migrations), starts in 24 h (count,
    failures, p50/p95 from `ops_events`), releases in 24 h by reason;
  - `hosts`: `vm_hosts` live, plus the latest 8 gone in 24 h (`gone24h` counts them all): kind, gateway, status,
    retiring, instance, launch time, last seen / answering, sandboxes on it / slots, memory, load, gone reason;
  - `scaler`: the vm scaler's plan (need, demand, waiting, available, pending, slots per host) and the
    `sandbox_demand:*` hints (counted within 2 min, or expired);
  - `events`: the latest 60 `vm_events` with the sandbox's owner;
  - `budget`: `budgetStatus` (limit, since, spent, left) and model spend today / this month / since the budget, in
    total, per owner (`egress_log.sandbox_id` → `sandboxes.owner`; old chat UI calls: their conversation's owner) and
    per model (`egress_log.model`, recorded since migration 11; older calls show as "(not recorded)");
  - `egress`: 24 h of model/route errors by status and decision (402 `deny:budget`, 503, …), blocked hosts (deny
    reason, count), the latest 30 refusals and errors;
  - `people`: per config user (and former users with sandboxes): conversations counted from their sandboxes (a
    conversation gets one at its first message): total, started today, last 7 days, live now, deleted, last active.

No message contents: owners, ids, hosts, counts, costs and timings only.

## Sandbox lifecycle

- **Made when the UI first runs Claude Code** in a conversation (`/internal/cc/sandbox`), at most one live sandbox per
  conversation (unique index `sandboxes_one_live`). The row records `owner` and `user_key` (`"u"` + the first 24 hex
  of sha256 of the lower-cased email): provider `vm` mounts JuiceFS `ws/<conversation>` as `/workspace` and
  `users/<user_key>/claude` as `~/.claude`, and gives the sandbox Claude Code's settings (docs/CLAUDE-CODE.md).
- **Kept while used:** installs, caches and background processes carry over between messages (and across a live
  migration on provider `vm`).
- **Released** after `sandbox.idleMinutes` (default 30) without a touch, and always at `sandbox.maxLifetimeHours`
  (default 8); at once when the conversation is deleted. Nothing is saved on release: `/workspace` and `~/.claude` are
  already on JuiceFS. The janitor (one replica at a time, every 30 s) also clears stuck starts and releases, removes
  sandboxes a provider lists without a live row, and marks rows whose sandbox the provider no longer has.
- **Pool:** at most `sandbox.maxActive` live sandboxes per instance; when full, the least recently touched sandbox
  idle for 2 minutes or more is released to make room; with none, the call waits up to 60 s, then fails with "the
  sandbox pool is full, try again in a minute".
- **No secrets in a sandbox.** Its env has only the proxy settings `HTTP_PROXY`/`HTTPS_PROXY`/`http_proxy`/
  `https_proxy` `=http://sbx:<egress token>@<egress>:3128` (`NO_PROXY` empty), the pip/npm mirror settings, its exec
  token (removed before any process starts) and Claude Code's settings (the egress token again as
  `ANTHROPIC_AUTH_TOKEN`, the model ids, the managed CLAUDE.md and MCP config, and
  `CLAUDE_CODE_ATTACHMENTS_BY_PATH=1`, which turns on Claude Code's SendUserFile: docs/CLAUDE-CODE.md "Sent files"). The model key stays in egress.
- Limits per sandbox: `sandbox.cpu` (default "1"), `sandbox.memory` (default "1Gi"); on `vm` a 5 GiB scratch for
  everything outside `/workspace` and a 1 GiB / 1,000-file quota on `/workspace` (docs/CLAUDE-CODE.md).
- **Installing software:** `pip install` (venv `/opt/py`, first on PATH, through the egress PyPI mirror), `npm install
  -g` (prefix `~/.npm-global`, npm mirror), installers into `~/.local/bin` / `~/.cargo/bin`; system packages with
  `sudo apt-get install -y x` on provider `vm`, where uid 1000 has passwordless sudo and root exists only inside
  gVisor (docs/SANDBOX-VM.md, "Installing packages").

## Sandbox providers (`sandbox.provider`)

The sandbox manager (`chat/src/web/sandbox.ts`) owns the lifecycle above and the `sandboxes` rows; a provider
(`chat/src/web/provider.ts`) only creates (`create` → its name for the sandbox, stored in `sandboxes.pod`), waits for
the agent (`waitReady` → `{url, headers}`, stored in `sandboxes.agent`), keeps alive, destroys and lists. The UI gets
the URL and headers from `/internal/cc/sandbox`, so it works the same with any of them. Only `vm` sets up Claude Code
(its env, `/workspace` and `~/.claude` on JuiceFS): `kubernetes` and `e2b` start the same image and agent but without
those, so they serve tests and a future instance, not YYC AI as it is.

- **`vm`** (`sandbox-vm.ts`, `vm-clients.ts`; host agent `chat/sandboxd`; the YYC instance since 2026-10-21): gVisor
  sandboxes (the DE0CH/gvisor fork) on our own hosts: one always-on Hetzner hub plus AWS EC2 burst hosts (at most
  `aws.maxHosts`), live-migrated between them (pre-copy) so the scaler can drain AWS hosts back onto the hub and
  terminate them. Full design, operations and measurements: docs/SANDBOX-VM.md.
  - **Create:** the controller (web) places the sandbox (the hub while it has room, else the fullest AWS host with
    room; none: the scaler starts one, `create` waits up to ~170 s, or answers "all sandbox hosts are full" at the
    cap) and gives it a fixed /32 from `sandbox.vm.sandboxCidr` (`sandboxes.ip`; `sandboxes.host` = where it runs
    now). sandboxd on that host (`POST /v1/sandboxes`, mutual TLS) starts it with the env (proxy and mirrors at
    `http://sbx:<egress token>@<egressVip>:3128`, `JARVIS_EXEC_TOKEN`, Claude Code's settings from
    `chat/src/web/claude-code.ts`) and `workspace` (the conversation id) and `user` (the owner's key), which sandboxd
    bind-mounts from the host's JuiceFS mount as `/workspace` and `~/.claude` (docs/CLAUDE-CODE.md). The sandbox's first process is
    `sbx-init.mjs` (root inside gVisor only, to start dockerd on the first `docker` command); the agent runs as uid
    1000 with no capabilities and no_new_privs.
  - **Ready:** `http://<ip>:7070/healthz` (no provider headers); the URL never changes, also across migrations.
  - **keepAlive:** a no-op; the janitor releases.
  - **Egress:** sandboxes reach egress at `sandbox.vm.egressVip`, which the hub translates (DNAT only) to an egress
    pod: egress sees the sandbox's own address, and egress only accepts a vm sandbox's token from that address. Each
    egress pod announces `POD_IP` in `kv egress_pod:<pod>`; the scaler hands the live ones to the hub.
  - **Janitor:** `list()` = every host's `GET /v1/sandboxes` (an unreachable host's rows count as present; the scaler
    decides when a host is lost). `sandbox.vm.legacyProvider: "kubernetes"` during a switch: the previous provider's
    sandboxes (rows without a host) stay listed and are removed through it once released.
  - **Scaler** (web, Postgres leader lock 4242012, every 5 s): heartbeats (a host silent 30 s, the hub 120 s, loses
    its sandboxes: rows `gone`, "host lost", `reset` on the next `/internal/cc/sandbox`), the hub's WireGuard peers and
    egress targets, AWS hosts up (spare slots) and down (drain-back, one sandbox at a time, also while Claude Code
    runs in it; a move across clouds only when the sandbox is idle and aborted, unchanged, if pre-copy wouldn't converge within
    `migration.maxDowntimeMs`), empty AWS hosts terminated. Operator knobs live in table `vm_hosts`
    (`max_sandboxes`, `status` `retiring`).
- **`kubernetes`** (`sandbox-k8s.ts`): a gVisor pod per sandbox in `sandbox.namespace`, agent at `http://<pod IP>:7070`,
  the env (proxy, mirrors, `JARVIS_EXEC_TOKEN`) in the pod spec, the proxy `sandbox.egressProxyUrl` (default
  `http://egress.<ns>.svc.cluster.local:3128`). No provider timeout: `keepAlive` is a no-op. `create` retries for up
  to a minute while a namespace ResourceQuota still counts a pod the pool just released (it terminates for a few
  seconds). Used by the tests (a fake Kubernetes API starting the real agent); YYC AI ran on it until 2026-10-21.
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
  - **Lifetime:** E2B kills a sandbox at its timeout. On every start and touch (at most every 30 s per sandbox) the
    manager sets it (`POST /sandboxes/{id}/timeout`) to the idle release + 5 minutes, capped at the hard maximum + 5
    minutes, so the janitor's release always comes first, and a sandbox whose web replicas died still ends. Release = `DELETE /sandboxes/{id}`.
  - **Janitor:** `GET /v2/sandboxes?metadata=jarvis=<instance>`; a sandbox older than a minute with no live row is
    killed. Other instances' sandboxes (another `jarvis` tag) are never touched.
  - **Plan limits:** E2B Hobby ends any sandbox after 1 hour of continuous running; `sandbox.maxLifetimeHours` above 1
    needs Pro (24 hours).
  - **Measured** (2026-10-20, us-west-1, with the retired server-side bash path): create 1.1 s, `/init` + health
    0.6 s; a pip install of a small package 6.4 s. Cloudflare refuses a
    request whose Host differs from the egress host's SNI (403), so the SNI allow-list can't be used to reach other
    sites behind Cloudflare.

## Interfaces

### Postgres schema (migrations in `chat/src/common/schema.ts`, applied under an advisory lock by every role at start)

```sql
create table sandboxes (id uuid primary key, conversation_id uuid not null, pod text not null unique,
  egress_token_hash text not null unique, exec_token text not null,
  status text not null check (status in ('starting','ready','releasing','gone')),
  created_at timestamptz not null default now(), last_active_at timestamptz not null default now(),
  expires_at timestamptz not null, released_at timestamptz, release_reason text,
  agent jsonb,                    -- {url, headers}: how to reach its agent (migration 2)
  host text, ip text,             -- provider vm: the host it runs on now, its fixed /32 (migration 3)
  owner text, user_key text);     -- its owner and ~/.claude key (migration 9)
create table vm_hosts (...);      -- provider vm: hosts (migration 3; docs/SANDBOX-VM.md)
create table vm_events (...);     -- provider vm: launches, packing, migrations with their pause, terminations
create table egress_log (id bigserial primary key, at timestamptz not null default now(), conversation_id uuid,
  sandbox_id uuid, turn_id uuid,  -- turn_id: old rows only
  kind text not null,             -- 'connect' | 'http' | 'mirror' | 'model' | 'inject'
  method text, host text not null, port int, path text, status int, decision text not null,  -- 'allow' | 'deny:<why>'
  bytes_up bigint, bytes_down bigint, ms int, cost_usd numeric,
  model text);                    -- kind model: the model the request asked for (migration 11)
create table tool_calls (id bigserial primary key, at timestamptz not null default now(), turn_id uuid,  -- old rows
  conversation_id uuid, user_email text, tool text not null, args jsonb, ok boolean, ms int, error text, cost_usd numeric);
create table ops_events (id bigserial primary key, at timestamptz not null default now(), kind text not null,
  ok boolean, ms int, sandbox_id uuid, conversation_id uuid, host text, detail jsonb not null default '{}');
  -- kind 'sandbox_start': ok, ms = start latency incl. pool wait, detail {reason, error, provider, startMs}; 30 days
create table kv (key text primary key, value jsonb not null);  -- cc_sandbox:<conv>, egress_pod:<pod>, sandbox_demand:<email>
```

The old chat UI's tables (`users`, `conversations`, `messages`, `turns`, `events`, `claw_sessions`, `files`,
`shares`, `blobs`, `blob_grants`, plus `confirmations` and `allowed_hosts`) keep their rows and are read and written by
nothing (schema 10 comments them "unused"); dropping them is a decision for Deyao.

### Tokens

- **Egress token**, per sandbox (32 random bytes, base64url), stored as `sandboxes.egress_token_hash`; valid while the
  sandbox is not `gone`. The sandbox's proxy, model and MCP calls carry it; egress and tools identify the sandbox (and
  its conversation and owner) by it.
- **Exec token**, per sandbox: the sandbox agent's own (`X-Jarvis-Exec-Token` or `Authorization: Bearer`), handed to
  the UI by `/internal/cc/sandbox`.
- **`JARVIS_CC_TOKEN`**: the UI pods → web.

### `egress` (port 3128, plus 9090 for /healthz and /metrics)

1. **Forward proxy for sandboxes.** `CONNECT host:port` and absolute-URI HTTP requests with
   `Proxy-Authorization: Basic base64("sbx:<egress token>")`. Unknown/expired token → 407. The host must pass the
   sandbox allowlist (below; `deny:host`, `deny:denylist`, `deny:config`), then resolve through the threat-intel
   resolvers (`deny:threat`, `deny:nxdomain`, `deny:dns`), then to a public address (`deny:address`: private,
   loopback, link-local and cluster addresses are refused after resolution, no SSRF into the cluster). Ports: 80, 443
   only (`deny:port`). A refusal is a 403 with a plain-text body: "egress: <host> is not on the sandbox's internet
   allowlist, and it cannot be enabled from the chat. … To read a web page, use the web_fetch and web_search tools
   instead of the sandbox." (or the DNS/address reason). Nobody is asked and nothing can grant a host. A tunnel or
   request that sends more than `egress.maxUploadBytes` (default 8 MiB) is cut (`deny:upload`): bulk upload is the
   exfiltration path, and installing packages or reading data sends little. Every request → `egress_log` with its
   decision (`allow`, `allow:cache` or `deny:<reason>`).
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
   (`{name, upstream, inject: {header: value-from-env-var}, model?, keepAuth?, allowPaths?}`), each authenticated by
   a live sandbox's egress token (`Authorization: Bearer`/Basic or `Proxy-Authorization`; a vm sandbox's only from its
   own address). The YYC instance has two. `claude` (`model: true`) → `https://openrouter.ai/api`: Claude Code's
   model calls (`ANTHROPIC_BASE_URL=http://<egress>:3128/x/claude`, `ANTHROPIC_AUTH_TOKEN` = the egress token);
   injects `Authorization: Bearer $OPENROUTER_API_KEY`, forces zero data retention into every request body (`provider.zdr: true`, `provider.data_collection: "deny"`, set over whatever the caller sent; a body that is
   not a JSON object is refused with 400, `deny:body`; no config turns this off: `forceZdr` in
   `chat/src/egress/proxy.ts`), streams responses through, logs `cost_usd` from
   the response's `usage.cost` (SSE: the final `message_delta` usage) into `egress_log` (kind `model`).
   The caller's `Authorization`, `x-api-key` and `Cookie` never go upstream; egress asks upstream for an
   uncompressed body (`accept-encoding: identity`) so it can read the cost.
   A model route also refuses, before anything goes upstream: a body whose `model` is not one of config `models`
   or `titleModel` (400 `invalid_request_error`, `deny:model`), and every call once the shared budget is used up
   (402 `{"type":"error","error":{"type":"budget_exceeded","message":"<budget message>"}}`, `deny:budget`; a model
   call's `egress_log` row is written as soon as it ends and the budget is re-read at most every 0.5 s; see "Budget").
   `tools` (`keepAuth: true`, `allowPaths: ["/mcp"]`) → `http://tools.<ns>.svc:8080`: Claude Code's MCP server
   (`/x/tools/mcp`); the caller's `Authorization` (its egress token) goes on, so the tools role knows the sandbox.
4. **Env:** `DATABASE_URL`, `JARVIS_CONFIG`, `JARVIS_CACHE_DIR` (default `/var/cache/jarvis-egress`, an emptyDir),
   `EGRESS_SELF_HOSTS`, `POD_NAMESPACE`, `EGRESS_BLOCK_CIDRS` (extra refused ranges, comma list: the node's own
   public /64 on YYC), `EGRESS_DRAIN_SECONDS` (default 120), and every variable a route's `inject` names
   (`OPENROUTER_API_KEY`). Identity lookups are cached for 5 s. At start it logs `egress allowlist` with the lists'
   version and sizes. On SIGTERM: `/healthz` turns 503, the listener closes, open requests and tunnels get up to the
   drain time, the log is flushed.

### The sandbox allowlist (`chat/egress-allowlist/`, `chat/src/egress/allowlist.ts`, `threatdns.ts`)

Which hosts a sandbox may reach is decided automatically; there is no per-conversation grant and no staff override.

- **Matching.** Entries are `host` (that name only) or `.domain` (the domain and every name under it). A host passes
  when its most specific allow entry is more specific than its most specific deny entry (an exact entry beats a
  suffix entry of the same name; equally specific → deny). So `.google.com` lets `www.google.com` through while
  `.drive.google.com` refuses Drive, and the exact `raw.githubusercontent.com` passes under the denied
  `.githubusercontent.com`. Config `egress.denyHosts` wins over everything (`deny:config`).
- **Allow = generated `allow.txt` + config `egress.defaultAllowedHosts`.** `allow.txt` is built by
  `chat/egress-allowlist/generate.py` (Python standard library) from:
  - `sources/dev.txt`: package registries, code hosts, toolchains, container registries, distro mirrors, merged from
    two lists made for AI coding sandboxes: OpenAI Codex cloud's "Common dependencies" preset
    (https://developers.openai.com/codex/cloud/agent-internet) and Claude Code on the web's "Default allowed domains"
    (https://code.claude.com/docs/en/cloud-environments), plus the hosts `docker pull` really uses (traced:
    `registry-1.docker.io`, `auth.docker.io`, `index.docker.io`, `production.cloudflare.docker.com`,
    `production.cloudfront.docker.com`, Docker's R2 bucket host; `ghcr.io`, `pkg-containers.githubusercontent.com`).
    Left out of both: catch-alls that contain upload services (`google.com`, `microsoft.com`, `azure.com`,
    `*.amazonaws.com`, `*.googleapis.com` storage, `*.r2.cloudflarestorage.com`, `gist.github.com`, telemetry intake).
    Domain names are facts; both sources are public documentation.
  - `sources/yyc.txt`: a short curated list of the news and data sources YYC staff use (HK/Macao/mainland government
    and statistics sites under `.gov.hk`, `.gov.mo`, `.gov.cn`, `data.gov.hk`, `censtatd.gov.hk`, `info.gov.hk`; HK,
    mainland and international news outlets; World Bank, IMF, OECD, UN, Wikipedia), from the hosts YYC's news-stream
    collects from.
  - **A ranked set: the top 50,000 registrable domains by the Tranco method** (Dowdall rule over popularity lists,
    Le Pochat et al., NDSS 2019, https://tranco-list.eu) computed over the providers whose terms allow any use: the
    Chrome UX Report (CC BY-SA 4.0, via github.com/zakird/crux-top-lists), the Majestic Million (CC BY 3.0) and the
    Cisco Umbrella top 1M (free of charge). The published Tranco list itself is not used: it also folds in Cloudflare
    Radar (CC BY-NC 4.0, non-commercial) and Farsight. Registrable domains by the Public Suffix List's ICANN section
    (MPL 2.0). Newly registered domains never make the ranked set.
  - **Minus risky categories** from the UT1 web classification (Université Toulouse Capitole, CC BY-SA 4.0,
    https://dsi.ut-capitole.fr/blacklists/): malware, phishing, cryptojacking, ddos, dialer, stalkerware, hacking,
    warez, adult, mixed_adult, filehosting, webhosting, redirector (also its strict/strong variants), shortener, proxy,
    vpn, residential-proxies, doh, dynamic-dns, webmail, chat, remote-control. A listed ranked domain is dropped; a
    listed subdomain of a kept domain goes on the deny list (adult: drop only).
- **Deny = generated `deny.txt`:** `sources/deny.txt` (curated: pastebins and code-sharing with anonymous upload,
  anonymous file drops and file sharing incl. Google Drive/Docs/Forms and Apps Script, public object storage,
  request catchers and webhooks incl. Discord/Slack/Telegram bot APIs, tunnels, anyone-runs-a-server hosting,
  form builders, webmail) + the UT1 subdomains above + every Public Suffix List **private-section** suffix under an
  allowed domain (user-content platforms where anyone gets a subdomain: `.workers.dev`, `.github.io`,
  `.herokuapp.com`, …). Upload to a place that takes it without an account of its own is the main exfiltration
  path a prompt-injected command could use, so these are refused even though they are popular.
- **Size and version (2026-10-21):** allow 48,664 entries (105 dev + 78 YYC curated; ranked: 48,856 kept of 50,000
  after 1,144 dropped by category, less the 269 the deny list names exactly, e.g. `.pastebin.com`, `.amazonaws.com`,
  CDN/platform domains such as `.akamaized.net`, `.azurewebsites.net`), deny 14,653. Each file's header carries `# version: <date> <digest>` and the sources;
  egress logs the version at start.
- **Refresh:** workflow `egress-allowlist` (Mondays) runs `generate.py`, commits to main only when an entry changed,
  and dispatches the `chat` workflow so the images carry the new lists; an instance takes them with its next jarvis3
  pin bump. Change the curated sets in `sources/` and run `python3 -I chat/egress-allowlist/generate.py` (about a
  minute and 100 MB of downloads; `--cache DIR` keeps them).
- **Threat-intel DNS on every connection:** egress resolves sandbox destinations through Cloudflare's malware-blocking
  resolvers (1.1.1.2, 1.0.0.2: a blocked name answers 0.0.0.0/::) with Quad9 (9.9.9.9: NXDOMAIN for a blocked name)
  as fallback, over plain DNS from the egress pod. 0.0.0.0/:: → `deny:threat`; NXDOMAIN/no address → `deny:nxdomain`;
  no resolver answering → `deny:dns` (fail closed). Answers are cached for their TTL (30 s – 5 min). The connection
  goes to exactly the checked address, after the private-address check.
- **Residual risk, accepted:** an allowed site that takes uploads with credentials the attacker supplies (an API
  token planted in injected content, e.g. for GitHub) is still reachable; the sandbox holds no secrets, staff data in
  the workspace is what such an upload could carry, and the upload cap bounds it.
- **The agent is told** in its managed CLAUDE.md (`SANDBOX_RULES`, `chat/src/web/claude-code.ts`): only sites on the
  allowlist are reachable, a refused site cannot be enabled, and web pages are read with the web search and fetch
  tools (server-side, any public site).

### `tools` (port 8080, `POST /mcp`, MCP Streamable HTTP, stateless, JSON responses)

- Auth: `Authorization: Bearer <egress token>` of a live sandbox made for a person (`sandboxes.owner`), passed on by
  egress's `tools` route → conversation and owner email. Anything else → 401.
- Every call → `tool_calls`.
- **Read-only tools only.** Nothing here sends, posts, writes or changes anything outside, and nothing asks for or
  waits on a confirmation. Tools (names lowercase, `snake_case`; annotations set `readOnlyHint`): `web_search`,
  `web_fetch`, `news_search` (Wisers), `social_search` (TikHub), plus the **listed** tools of each configured upstream
  MCP server, exposed as `<prefix>_<tool>` (YYC: prefix `yyc`, the yyc-mcp Worker, service token +
  `X-YYC-User: <owner email>`).
- **Upstream tools: an explicit read-only allowlist, default deny.** `tools.upstreams[].tools` names the upstream's
  tools (their own names, without the prefix) that may be offered: search/get/list-style tools only. A tool not on
  the list is hidden (a new upstream tool stays hidden until someone adds it), and so is a listed tool the upstream
  marks `readOnlyHint: false` or `destructiveHint: true`. An upstream without a `tools` list offers nothing. Hidden
  tools are not in `tools/list` and `tools/call` answers "unknown tool" for them; each refresh logs the hidden names
  (`upstream tools … hidden`). Anything that sends mail, posts or mutates (e.g. a `send_mail`) must never be listed.
- A built-in tool whose provider is not configured is left out of `tools/list` (`web_search` needs `tools.exa`,
  `news_search` `tools.wisers`, `social_search` `tools.tikhub`; `web_fetch` without Exa reads pages directly).
  `web_fetch`'s direct path refuses non-public addresses (checked after DNS, connecting to the checked address) and
  ports other than 80/443/8080/8443. `tools.tikhub.platforms` (optional) limits `social_search` to a subset of
  `xiaohongshu, douyin, weibo, x, threads, youtube, instagram, reddit`. Upstream tool lists are read at startup and
  every 10 minutes as the first admin (`${USER_EMAIL}` = that admin while listing, the sandbox's owner on calls).
- Env: `PORT` (8080), `JARVIS_DRAIN_MS` (150 s: an upstream call, up to 2 minutes, finishes before the pod stops;
  `terminationGracePeriodSeconds` 180), plus `DATABASE_URL` and the key env vars the config names.

### The sandbox agent (`chat/sandbox/agent.mjs`, port 7070)

The only process the server side talks to inside a sandbox; runs as uid 1000.

- `GET /healthz` → 200 `{ok:true, boot_id}` (no token needed); 503 while it waits for `/init`. `GET /livez` → 200.
- Auth: the exec token in `X-Jarvis-Exec-Token` (survives a provider's port proxy) or `Authorization: Bearer`.
- `GET /proc` with `Connection: Upgrade`, `Upgrade: jarvis-proc`, `X-Proc: <base64 JSON {args, cwd?, env?}>` → 101,
  then frames both ways (1 byte type, 4 bytes big-endian length, payload). Client → agent: 0 stdin, 4 stdin end, 5
  signal (its name). Agent → client: 1 stdout, 2 stderr, 3 exit (`{code, signal}`), then the socket closes. `args[0]`
  is looked up on PATH; `cwd` must be `/workspace` or a folder in it (made if missing; default `/workspace`); `env` is
  added for that process only. A dropped connection (close or FIN) kills the process group (SIGTERM, SIGKILL 2 s
  later): the caller owns its life. This is how the UI runs `claude` in the sandbox.
- `POST /init` (only with `JARVIS_INIT=1` and no `JARVIS_EXEC_TOKEN`, once, no token; provider `e2b`): `{token, env,
  wire?: {url, headers}}` sets the exec token, adds `env` to every process's environment and, with `wire`, starts the
  local proxy on `127.0.0.1:3128` that carries each connection to egress's `/wire` over a WebSocket. A second call
  answers 409.
- Nothing else is served (404).

### `web` deployment needs

- Env: `JARVIS_ROLE=web`, `DATABASE_URL`, `JARVIS_CC_TOKEN`, `JARVIS_NAMESPACE` (default `jarvis`),
  `JARVIS_DRAIN_SECONDS` (default 300). Config at `/etc/jarvis/config.json` and the system prompt file it names (the
  managed CLAUDE.md starts with it).
- Provider `vm`: the `sandbox-vm` Secret's PEMs and AWS key; `JARVIS_JUICEFS_META_URL`/`_PASSWORD`; egress to the
  sandbox hosts (sandboxd :7443, agents :7070). Provider `kubernetes`: a ServiceAccount bound to a Role in the sandbox
  namespace (`pods`: create, get, list, watch, delete; `pods/status`: get). Provider `e2b`: `E2B_API_KEY` and any
  variable `sandbox.e2b.egressHeaders` names.

## Deploying an instance

The base is `deploy/chat/` (Kustomize): namespace `jarvis` (Pod Security restricted) with `web`, `egress`, `tools`
(2 replicas each, rolling with `maxUnavailable: 0`, PDBs, drained on SIGTERM: grace 330 / 150 / 180 s), the tunnel
connector `cloudflared`, NetworkPolicies (ingress: tools ← egress, egress:3128 ← cloudflared (the wire), nothing else;
egress: DNS, the namespace's pods, public internet only) and a LimitRange. The instance adds its UI (YYC:
`yyc-ai/deploy/ui.yaml`, image `ghcr.io/de0ch/yyc-ai-ui`), the policy admitting the UI pods to web, and its sandbox
hosts' policies. The image is the placeholder `jarvis3-chat`.

An instance is a Kustomization (YYC: `DE0CH/YYC` `yyc-ai/deploy/`) that
- uses `github.com/DE0CH/jarvis3//deploy/chat?ref=<commit>` as its base and maps `jarvis3-chat` to
  `ghcr.io/de0ch/jarvis3-chat:<sha12>` with `images:` (the sandbox image tag goes in `config.json`);
- generates the ConfigMap `jarvis-config` (`config.json`, `system-prompt.md`);
- supplies Secrets `web-env` (`DATABASE_URL`, `JARVIS_CC_TOKEN`), `egress-env` (`DATABASE_URL`,
  `OPENROUTER_API_KEY`), `tools-env` (`DATABASE_URL` + the tool keys its config names), `tunnel` (`token`), plus a
  `ghcr-pull` docker-registry Secret patched onto the Deployments while the GHCR packages are private;
- defines the PriorityClass `serving` the base's pods use (or patches `priorityClassName`).

A new version = bump the base `ref` and the image tags (chat image in `images:`, the sandbox's in `config.json`
`sandbox.image`) to a commit whose `chat` workflow pushed images, and push; Flux rolls the Deployments one pod at a
time. A new sandbox image reaches new sandboxes only (each host fetches it once); running ones keep theirs.

## Budget (`budget`, optional; absent = no limit; `chat/src/common/budget.ts`)

One limit for everyone, admins included. Spend = `sum(egress_log.cost_usd)` of kind `model` (every model call Claude
Code makes, its small/fast-model calls too) with `at >= since`. Spent ≥ `limitUsd` = used up: egress refuses every
model call with 402 `{"type":"error","error":{"type":"budget_exceeded","message":"<message>"}}` (`deny:budget`). A
call already in flight when the limit is crossed completes, so the spend can end above the limit by about one model
call per running conversation. Message (`product.locale`): zh-HK「<ownerName> 設定了 <limit> 美元的使用上限，目前已用完。」,
English "<ownerName> has set a USD <limit> budget limit and it has now been used up." (no `ownerName`: 管理員 / The
administrator). There is no automatic reset. To reset or raise: edit `budget` in the instance's config.json (`since`
= now resets the spend to 0; a bigger `limitUsd` raises it) and roll out the config. Check:
`select sum(cost_usd) from egress_log where kind = 'model' and at >= '<since>'`.

## Config (`/etc/jarvis/config.json`, from the instance)

```json
{
  "product": {"name": "YYC AI", "locale": "zh-HK"},
  "publicUrl": "https://ai.yyc.deyaochen.com",
  "access": {"teamDomain": "de0ch.cloudflareaccess.com", "audience": "<aud tag>"},
  "users": ["chendeyao000@gmail.com"], "admins": ["chendeyao000@gmail.com"],
  "models": [{"id": "anthropic/claude-sonnet-5.5", "label": "Sonnet 5.5"}],
  "defaultModel": "anthropic/claude-sonnet-5.5", "titleModel": "anthropic/claude-sonnet-5.5",
  "systemPromptFile": "/etc/jarvis/system-prompt.md",
  // provider "vm" (docs/SANDBOX-VM.md "Config"): our own gVisor hosts, the only one that runs Claude Code
  "sandbox": {"provider": "vm", "image": "ghcr.io/de0ch/jarvis3-sandbox:<tag>", "idleMinutes": 30,
              "maxLifetimeHours": 8, "maxActive": 10, "cpu": "1", "memory": "1Gi",
              "vm": {"hub": {"name", "url": "https://10.201.0.1:7443", "endpoint": "<hub IPv6>:51821"}, "cpuFeatures": "…",
                     "spareSlots": 0, "legacyProvider"?: "kubernetes", "aws"?: {"launchTemplate", "maxHosts": 3, …}}},
  // provider "kubernetes" instead: {"provider": "kubernetes", "image": …, "namespace": "jarvis-sandbox",
  //   "runtimeClass": "gvisor", "cpu": "1", "memory": "1Gi", "egressProxyUrl"?: …}
  // provider "e2b" instead: {"provider": "e2b", "e2b": {"template": "jarvis3-sandbox:<tag>",
  //   "egressUrl": "wss://egress.ai.yyc.deyaochen.com/wire", "egressHeaders": {}, "apiKeyEnv": "E2B_API_KEY", "instance": …}}
  // the sandbox allowlist ("The sandbox allowlist"): the generated lists (generatedAllowlist, default true) plus
  // defaultAllowedHosts; denyHosts wins over everything; maxUploadBytes per tunnel/request (default 8 MiB)
  "egress": {"defaultAllowedHosts": [], "denyHosts": [], "generatedAllowlist": true, "maxUploadBytes": 8388608,
             "routes": [{"name": "claude", "upstream": "https://openrouter.ai/api", "model": true,
                         "inject": {"Authorization": "Bearer ${OPENROUTER_API_KEY}"}},
                        {"name": "tools", "upstream": "http://tools.jarvis.svc.cluster.local:8080", "keepAuth": true,
                         "inject": {}, "allowPaths": ["/mcp"]}], "cacheGiB": 5},
  "tools": {"exa": {"keyEnv": "EXA_API_KEY"}, "tikhub": {"keyEnv": "TIKHUB_API", "platforms": ["xiaohongshu", "douyin", "weibo", "x", "threads", "youtube", "instagram", "reddit"]},
            "wisers": {"url": "https://yyc.deyaochen.com/app/wisers/api", "accessClientIdEnv": "WISERS_ACCESS_CLIENT_ID", "accessClientSecretEnv": "WISERS_ACCESS_CLIENT_SECRET"},
            "upstreams": [{"prefix": "yyc", "url": "https://yyc-mcp.yyc-news.workers.dev/mcp",
                           "headers": {"Authorization": "Bearer ${YYC_MCP_TOKEN}", "X-YYC-User": "${USER_EMAIL}"},
                           // read-only tools to offer (default deny; "tools" above): the upstream's own names
                           "tools": ["posts_search", "post_get", "streams_list"]}]},
  "budget": {"limitUsd": 30, "since": "2026-10-21T12:00:00Z", "ownerName": "Deyao"}
}
```

**Who and which models:** `users` (lower-cased) is who may sign in (the UI's router checks it; web checks
`/internal/cc/sandbox`'s owner against it); `admins` may open the operator page (router and web check it) and the
first admin is the identity the tools role lists upstream tools as. `access` is the Cloudflare Access app the UI's router verifies. `models`/`defaultModel`/`titleModel`: egress's
model routes refuse any other id (400, `deny:model`); `defaultModel` is Claude Code's model in the sandbox
(`ANTHROPIC_MODEL`), `titleModel` its small/fast model.

Secrets come from the environment (`DATABASE_URL`, `JARVIS_CC_TOKEN` (web), `OPENROUTER_API_KEY` (egress only), tool
keys (tools only); provider "vm", web only: `SBX_CA_CERT`, `SBX_CLIENT_CERT`, `SBX_CLIENT_KEY`, `SBX_HOST_CERT`,
`SBX_HOST_KEY` (PEMs), the scaler's least-privilege AWS key `SBX_AWS_ACCESS_KEY_ID`, `SBX_AWS_SECRET_ACCESS_KEY`, and
`JARVIS_JUICEFS_META_URL`/`JARVIS_JUICEFS_META_PASSWORD` for the burst hosts' user data). Each role gets only the
Secret it needs.

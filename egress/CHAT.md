# Jarvis 3 chat runtime

The server side of a staff AI chat instance (YYC AI, `ai.yyc-internal.com`): **Claude Code runs in a sandbox per
conversation**, and the staff UI is a fork of CloudCLI (DE0CH/claudecodeui) that the instance deploys itself. The
design and why: docs/CLAUDE-CODE.md. This runtime provides the sandboxes and everything between them and the outside:
the code is in `chat/`, its Kustomize base in `deploy/chat/`. An instance pins a commit of this repo and supplies its
config and secrets. It is a second runtime next to the task runtime in `server/` (OpenCode in Fly machines).

**What it is (Deyao, 2026-10-21): a read-only data-analysis tool, not an assistant that acts.** It searches, reads
and analyses (news, social posts, the instance's own data, uploaded files, a sandbox for code and data work); it never
sends, posts or changes anything outside. So nobody is ever asked anything: every tool is read-only (outward actions
such as sending mail are not offered at all), and the sandbox's internet access is decided automatically by an
allowlist with no manual override.

## Components (one image `ghcr.io/de0ch/jarvis3-chat`, role chosen by the first argument: `args: [egress]`)

| Role | What | Replicas |
|---|---|---|
| `web` | `/internal/cc/*` for the UI pods: the sandbox manager, its janitor and (provider `vm`) the scaler. No public route | 2, rolling, drained |
| `egress` | The sandbox gateway (aviary's contract): hermit's sandbox tokens checked locally, the fixed names `magpie`, `hermit`, `model` (OpenRouter with forced ZDR and the budget), `tools`; the package mirror; the automatic sandbox allowlist with threat-intel DNS; its own database `egress` | 2, rolling, drained |
| `tools` | MCP server `tools` (Streamable HTTP, JSON responses) for aviary's gateway: web search/fetch, social search, AnyAPI, upstream MCP tools; read-only tools only; owner from `Aviary-Sandbox-Person`, liveness from hermit; its own database `tools` | 2, rolling, drained |

Plus the sandbox image `ghcr.io/de0ch/jarvis3-sandbox` (Claude Code, Python/Node tools, Docker, the sandbox agent),
run by our own sandbox hosts (`sandbox.provider: "vm"`: gVisor, AWS burst hosts behind a gateway on a cluster node,
live migration; docs/SANDBOX-VM.md). See "Sandbox providers" for the others.

```
browser ─Access─▶ UI pod (router → one CloudCLI server per person; DE0CH/claudecodeui)
                    │ POST /internal/cc/sandbox {owner, conversation} ─▶ web ─▶ sandbox manager ─▶ provider (vm: sandboxd)
                    │ GET /proc (upgrade, exec token) ─▶ sandbox agent :7070 ─▶ `claude` (cwd /workspace)
                    ▼                                                              │ all traffic (HTTP(S)_PROXY)
                  /jfs (JuiceFS: ws/<conversation>, users/<key>/claude)            ▼
                          egress :3128 (hermit's sbx token checked locally, liveness from hermit)
                            ├─ http://magpie/v1/sandbox/** ─▶ magpie   (pair bearer, verified identity)
                            ├─ http://hermit/v1/sandbox/** ─▶ hermit   (pair bearer, verified identity)
                            ├─ http://model/v1/messages ─▶ OpenRouter (key injected, ZDR forced, budget)
                            ├─ http://tools/mcp ─▶ tools ─▶ Exa / TikHub / Wisers relay / yyc-mcp
                            ├─ self hosts (selfHosts, 127.0.0.1, localhost) /mirror/… ─▶ PyPI / npm mirror
                            └─ anything else ── sandbox allowlist → threat DNS → public internet
State: Postgres (sandboxes, vm hosts and events, tool_calls, kv); egress's own database `egress` (egress_log,
budget_state, token_keys); /workspace and ~/.claude on JuiceFS.
```

## Control plane (`web`, `/internal/cc/*`)

Cluster-only (the instance's NetworkPolicy admits its UI pods to web:8080) and authenticated by the shared bearer token
`JARVIS_CC_TOKEN` (401 otherwise); POST with a JSON body; `conversation` must be a uuid (400; optional only on `warm`).

- `POST /internal/cc/sandbox {owner, conversation}` → `{id, url, token, headers}`: the conversation's live
  sandbox, made if there is none, for `owner` (403 unless in config `users`). `url` + `headers` reach its agent,
  `token` is the agent's exec token. A new sandbox after a release is not announced to the model: the managed
  CLAUDE.md already says that only `/workspace` and the conversation carry over.
  Concurrent calls in one replica share one start; a start takes up to 3 minutes (a host booting).
- `POST /internal/cc/touch {conversation}`: it is in use; its idle time starts again. The UI touches every minute
  while Claude Code runs in it.
- `POST /internal/cc/release {conversation}`: the conversation was deleted; its sandbox goes at once.
- `POST /internal/cc/warm {owner, conversation?}` → `{ok, hinted}`: `owner` is typing (`conversation` absent for a
  new chat; 403 unless in `users`). Nothing is made and no idle time is reset; when the conversation has no live
  sandbox it records kv `sandbox_demand:<owner>`, which the vm scaler counts for 2 minutes and starts an AWS host
  for when no host has a free slot (docs/CLAUDE-CODE.md, "Warming").
- `POST /internal/cc/view-as {event, admin, user, session, conversation?, chars?}` → `{ok}`: one record of "View as"
  (below): `event` `start` (from the UI router), `message` or `conversation` (from the person's CloudCLI process, before
  it sends; `conversation` required). 403 unless `admin` is in `admins` and `user` in `users`. The caller goes on only
  after a 200.
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

- `GET /internal/ops/` the page (self-contained HTML, strict CSP; polls `snapshot` every 10 s while visible). It keeps
  two halves apart: **Now** (is everything fine) and **History** (a log, newest first, Hong Kong time). A phone shows
  them as tabs (`#now`, `#history`), a screen 1100 px or wider side by side. The header's pill is the status.
- `GET /internal/ops/snapshot?tz=<IANA zone>` → JSON (cached 4 s per zone; "today"/"this month" start at that zone's
  midnight, default Asia/Hong_Kong; the page asks for Asia/Hong_Kong). Each section is computed on its own (8 s
  timeout); a failed one is `null` with its message in `errors.<section>`.
  Now:
  - `health`: `{state, findings}`, state `ok` / `warming` (a host booting or retiring, a sandbox starting, a role
    rolling out) / `watch` (budget at 80 % or more) / `problem` (a role with fewer Ready pods than wanted, the gateway or
    a host not answering, a host booting over 5 min, a sandbox starting over 3 min, the budget used up, failed sandbox
    starts / model or route errors / host errors in the last 15 min, a section that could not be read);
  - `service`: the Deployments web, egress, tools, ui, cloudflared (desired, Ready, updated, rolling) and their pods
    (Ready, phase or waiting reason, restarts, age, node), read from the Kubernetes API with Role `web-ops-read`
    (`deploy/chat/web.yaml`); the instance adds a NetworkPolicy letting web reach the API server (YYC:
    `yyc-ai/deploy/ops.yaml`). `available: false` outside a cluster;
  - `hosts`: `vm_hosts` that exist (not gone): kind, gateway, status, instance, launch time, last seen / answering,
    sandboxes on it / slots, cap, memory, load, gVisor release;
  - `sandboxes`: live rows (conversation, owner, host, status, age, last active, migrations);
  - `scaler`: the vm scaler's plan (need, demand, waiting, available, pending, want, slots per host) and the
    `sandbox_demand:*` hints counted now (typed within 2 min);
  - `budget.status` and `budget.total`: `budgetStatus` (limit, since, spent, left), spend today and this month;
  - `recent`: the last 15 minutes: failed sandbox starts, model/route errors, host errors (`vm_events` kinds with
    fail/lost or an `error`), blocked requests (counted only), and the latest 20 of them.
  History:
  - `starts`: sandbox starts (`ops_events`: took how long, failed why) and releases (`sandboxes.released_at`: lived how
    long, why) over 24 h, newest first, with the summary (count, failures, p50/p95) and releases by reason;
  - `events`: the latest 60 `vm_events` with the sandbox's owner;
  - `hostsGone`: hosts gone in 24 h (latest 30; `count24h` counts them all) with reason and lifetime;
  - `egress`: 24 h of model/route errors by status and decision (402 `deny:budget`, 503, …), blocked hosts (deny
    reason, count), the latest 30 refusals and errors;
  - `budget.byDay` (14 days, days in the asked zone), `budget.byUser` (`egress_log.sandbox_id` → `sandboxes.owner`;
    old chat UI calls: their conversation's owner), `budget.byModel` (`egress_log.model`, recorded since migration 11;
    older calls show as "(not recorded)"): today / this month / since the budget;
  - `people`: per config user (and former users with sandboxes): conversations counted from their sandboxes (a
    conversation gets one at its first message): total, started today, last 7 days, live now, deleted, last active.
    The page gives each current user a 「View as」 button (below);
  - `viewAs`: the latest 100 view-as records (`ops_events` kind `view_as`, kept for good): when, which admin, as whom,
    started viewing / sent a message (its length) / started a conversation, the conversation and the view's id.

No message contents: owners, ids, hosts, counts, costs and timings only.

## View as (Deyao, 2026-10-28)

An admin sees YYC AI exactly as a staff member does (their conversation list, conversations, files, shares), in a tab
of its own, and can send as them only through a marked override. Staff are not told; every view and every send is in
the operator page's History (`viewAs`, above), and only there.

- **Start**: the page's 「View as」 is a form POST (new tab) to the UI router's `/api/jarvis/view-as` (`user`). The
  router (DE0CH/claudecodeui `jarvis/router.mjs` "View as") accepts it only from an Access-verified admin and the same
  origin, records `start` here (nothing opens if that fails), and answers 303 to `/#jarvis-view-as=<token>`: an HMAC
  token (key derived from `JARVIS_CC_TOKEN`, 12 h) naming the admin, the person and the view.
- **Enforcement** (router, every request and socket): a token counts only when the request's own Access identity is
  that admin and still in `admins`, and the person is still a user; any other request carrying a token is 403, so a
  non-admin can never become anyone. The request then goes to the person's own CloudCLI process as them, with
  `X-Jarvis-Viewer` (set only by the router). While viewing: GET anything; POST only warming and uploads; WebSocket
  only the Paseo bridge, which records each `message` and `conversation` here before sending it (nothing is sent
  without the record) and refuses renames, deletes and restores.
- **The tab** (the Paseo app, `yyc/view-as.ts`): keeps the token in that tab only and sends it on every call; the send
  button and the new-conversation buttons are disabled; one box only the admin sees, in English, holds the override
  (off on every load; red while on). Messages sent with it look exactly like the person's own.

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
    pod: egress sees the sandbox's own address, and egress only accepts a vm sandbox's token from that address.
    *Old stack only* (the egress pinned at image `f91907d349e6`, deploy/chat/egress.yaml): each egress pod announces
    `POD_IP` in `kv egress_pod:<pod>` and the scaler hands the live ones to the hub. The new egress announces nothing:
    in the new stack the VIP's targets are hermit's fabric (ready egress endpoints).
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
create table kv (key text primary key, value jsonb not null);  -- sandbox_demand:<email>; egress_pod:<pod> (old-stack egress only)
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

### `egress`: the sandbox gateway (`node src/main.ts egress`; ports 3128 proxy, 8082 in-cluster API, 9090 health)

egress is YYC's implementation of the HTTP half of aviary's sandbox gateway contract (DE0CH/aviary platform spec
3.11; the network half, the WireGuard hub and the sandbox-facing VIP on port 3128 translated to ready egress pods, is hermit's
fabric). Every sandbox client (the supervisor, Claude Code, pip, npm, curl) sends absolute-form requests or
`CONNECT host:443` to the proxy in its boot environment, `http://sbx:<sandbox token>@<vip>:3128`. Each request is
decided in this order:

1. **The credential.** `Proxy-Authorization: Basic base64("sbx:" + token)` (on self-host paths also `Authorization`,
   which pip and npm send from the index URL). Missing: 407 with `Proxy-Authenticate: Basic realm="aviary-gateway"`.
   The token is hermit's `hst1.<kid>.<payload>.<sig>`, checked locally (`chat/src/egress/tokens.ts`): the kid in
   hermit's key set (`GET http://hermit/v1/sandbox-token-keys`, refreshed every 5 min and at most every 10 s on an
   unknown kid, the last set kept when a fetch fails and stored in table `token_keys`), the Ed25519 signature, the expiry, and the connection's source
   IPv4 equal to the token's (unless it carries 0.0.0.0). A token naming a kid the set lacks waits for at most one
   refetch: one at once unless an unknown-kid refetch began in the last 10 s (or it joins the one running), else the
   403 is at once; the unknown-kid clock is its own, so the 5-minute refresh never delays it. During a hermit key
   rotation a refetch can land on a replica that does not publish the new kid yet: the client's retry after the 10 s
   mark refetches again. A fetched set with no usable key (empty, or only keys of another algorithm or length) is a
   failed fetch: the last set stays in use and in `token_keys`. A failure is final: 403 `{"code":"not_allowed"}` (JSON on a
   fixed name, plain text otherwise). Then liveness: `GET http://hermit/v1/sandbox-tokens/{sandboxId}` with
   `EGRESS_TOKEN_HERMIT`, 1 s timeout, cached 30 s and refreshed in the background; `live: false` or 404 → 403 within
   30 s. **A hermit problem never refuses a valid token:** while hermit fails the last answer is served whatever its
   age, and a sandbox never looked up yet is admitted while the lookup is retried.
2. **The fixed names** `magpie`, `hermit`, `model`, `tools` (exact host, no port or port 80, never DNS). The client's
   `Authorization`, `Proxy-Authorization`, `x-api-key` and every `Aviary-*` header except `Aviary-Protocol` and
   `Aviary-Lease` are stripped; `Aviary-Sandbox-Id`, `-Conversation`, `-Person` (the lowercased email) from the
   verified token, `Aviary-Request` (the client's if a lowercase uuid v7, else new) and the callee's bearer are added.
   - `magpie`, `hermit` (built in, not configured): `/v1/sandbox/**` only (else 403; dot or encoded-dot paths never
     routed) to `http://magpie` / `http://hermit` with `EGRESS_TOKEN_MAGPIE` / `EGRESS_TOKEN_HERMIT`; bodies up to
     64 MiB (64 MiB + 1: 413 `too_large`); streamed both ways over a kept-alive pool; an upstream silent 120 s ends
     the request. The hot path: no `egress_log` row, counters `egress_fixed_requests_total{route,status}` and
     `egress_fixed_bytes_total{route,dir}` instead; a gateway-side failure (502 unreachable, 504 idle, an answer cut)
     is logged (`egress fixed-name upstream`: route, status, request, conversation, `person_key`, never the path),
     at most one line per route per second with the left-out count in the next.
   - `model`: `POST /v1/messages`, `POST /v1/messages/count_tokens` to egress.json's route `model` (OpenRouter), and
     `GET /v1/aviary/models` answered by egress itself (below). The route injects `Authorization: Bearer
     $OPENROUTER_API_KEY` and **forces zero data retention into every request body** (`provider.zdr: true`,
     `provider.data_collection: "deny"`, set over whatever the caller sent; a body that is not one valid JSON object
     is refused with 400, `deny:body`; no config turns this off: `forceZdr` in `chat/src/egress/modelbody.ts`, called
     from `proxy.ts`), applies the configured thinking level, refuses a model outside `models[].id`, `defaultModel`,
     `smallModel`, `titleModel` (400 `deny:model`) and every call once the budget is used up (402 `budget_exceeded`,
     `deny:budget`), streams the answer back and logs its `usage.cost` (kind `model`). No `Aviary-*` header (no staff
     identity) reaches OpenRouter. **Memory:** a request carries the whole conversation, base64 images included; the
     body is read once into one Buffer and rewritten as slices of it plus the few rewritten members (about its own
     size), at most 64 MiB of model bodies held at once (a call waits its turn), a body over 64 MiB refused with 413
     `request_too_large` before it is read. No total timeout (Claude Code's own 600 s bounds it); an upstream silent
     120 s ends the call (504 `upstream_timeout` before the answer's head, the stream cut after it).
   - `tools`: `/mcp` to egress.json's route `tools` (`http://tools`, `Authorization: Bearer $EGRESS_TOKEN_TOOLS`),
     timeout 180 s; tools learns the owner from `Aviary-Sandbox-Person`. One log row per call (kind `tools`).
3. **Self hosts** (`selfHosts` in egress.json, plus `127.0.0.1` and `localhost`; any port; also origin-form requests to
   the proxy): local paths only, the **package mirror** (cache on the pod's volume, LRU at `cacheGiB`):
   `GET /mirror/pypi/simple/...` (pypi.org, links rewritten to `/mirror/pypi/files/...` = files.pythonhosted.org),
   `GET /mirror/npm/...` (registry.npmjs.org, tarball URLs rewritten to `http://<the request's Host>/mirror/npm/…`);
   immutable artefacts cached, index pages for 5 minutes. Anything else: 404. A self host never reaches the internet
   checks, so the VIP is never refused as a private address.
4. **The internet**: ports 80 and 443 (`deny:port`), the sandbox allowlist (below; `deny:host`, `deny:denylist`,
   `deny:config`), the threat-intel resolvers (`deny:threat`, `deny:nxdomain`, `deny:dns`), public addresses only
   (`deny:address`; private, loopback, link-local, cluster and `blockCidrs` ranges refused after resolution). A refusal
   is a 403 with a plain-text body: "egress: <host> is not on the sandbox's internet allowlist, and it cannot be enabled
   from the chat. … To read a web page, use the web_fetch and web_search tools instead of the sandbox." Plain-HTTP
   GETs to `deb.debian.org`/`security.debian.org` are cached (apt). **The upload cap** (`maxUploadBytes`, 8 MiB) counts
   only internet traffic, a CONNECT tunnel's or a forward request's client-to-upstream bytes (`deny:upload`, the
   connection cut): the fixed names and the mirror are never counted against it, so the supervisor's kept-alive
   connection carrying transcript writes and uploads is never cut.

Every request except the magpie and hermit calls → one `egress_log` row (kind `connect`, `http`, `mirror`, `model`,
`tools`; decision `allow`, `allow:cache` or `deny:<reason>`; the sandbox, conversation, `person` and `request`).

**In-cluster API (8082)**, callers by pair token (`EGRESS_ACCEPT_PIGEON`, `_HERMIT`, `_ADMIN`; comma lists, current
first; 401 without one, 403 `wrong_caller` for the wrong one): `GET /v1/aviary/models` (pigeon, admin; Service
`model`) → `{"default", "small", "title", "contextTokens", "models": [{"id", "label", "reasoning"}]}` from egress.json,
the same answer sandboxes get at `http://model/v1/aviary/models`; `GET /v1/ops/summary?tz=&recentMinutes=` (hermit,
admin): spend and the budget, refusals and errors (aviary platform spec 15.3; owners are emails);
`GET /v1/admin/egress-log?conversation=&since=&limit=` (admin): one conversation's rows.

**Health (9090):** `/healthz`; `/readyz` 503 until migrations are verified, the allowlist is loaded and tokens can be
verified (hermit's key set fetched once, or, while hermit is away at this start, the set stored in `token_keys`), and
while draining (not when Postgres or hermit fail later); `/version` (`builtAt` from the image's `CHAT_BUILT_AT`);
`/metrics` (the counters above, `egress_log_buffered`, `egress_log_dropped_total{kind}`,
`egress_drain_cut_total{kind}`, `egress_in_flight{kind}`, `egress_token_keys_from_store`,
`egress_budget_unknown_served_total`).

**Its own database `egress`** (`EGRESS_DATABASE_URL`; `chat/src/egress/db.ts`, migrations under egress's own lock):
`egress_log` (no retention: the budget sums every model row since `budget.since`), `budget_state` (the last budget
status a replica read) and `token_keys` (hermit's last key set). **A database outage never stops a running turn:** the
budget gate serves its last known status plus the model costs this replica has logged since, so a replica does not
overspend on stale data; a call waits for a budget read at most 250 ms after that read began and not at all while
reads fail, so a database that hangs rather than refuses costs one such wait at its start; with no status ever known
(no `budget_state` row, every read failed) it counts this replica's own costs from zero against the limit (counted,
logged). Log rows wait in memory (at most 50,000, the oldest non-model row dropped first when full, counted; never a
model row) and are written in order once the database is back. A row is cleaned before it is buffered (port 1-65535
or null, no NUL in text, text capped, a bad uuid null), and a row Postgres still refuses as data (SQLSTATE class 22 or
23) is a bad row, not an outage: its batch is written row by row, that row dropped (logged, counted in
`egress_log_dropped_total{kind}`) and the rest written in order. **Migrations** are expand-only: a database at a newer
migration than the build knows is served (a rollback, or an old pod restarting mid-rollout); each migration's sha256 is
recorded, and a recorded migration whose SQL this build has differently is the one start failure (exit 2).

**Drain** (SIGTERM, after Kubernetes' 10 s preStop): `/readyz` 503, no new connections, the kept-alive connections
idle at that moment closed, every answer from then on carries `Connection: close` (a new request on a kept-alive
connection, such as one whose answer began before the drain, is served that way; a connection that goes idle later is
not closed under its client); model streams and tunnels finish up to 600 s,
magpie/hermit requests up to 60 s, tools calls up to 180 s; what is left at its bound is cut and counted in
`egress_drain_cut_total{kind}`; the log is flushed, retried every second for what the grace period has left (at least
10 s); exit 0 with log `drained` (and the cuts). A second SIGTERM exits at once. Grace 625 s.

**Known gaps** (aviary refactor WP-25 review):
- Rows still unwritten when the pod is killed are lost (a rollout or eviction during a database outage longer than the
  grace left after the drain): logged as `egress log lost at exit` with their count and model cost. The shared budget
  then undercounts that cost for good and the calls are missing from the ops summary. The pod's cache is an emptyDir
  that goes with the pod, so there is nowhere local to keep them.
- Readiness waits for a usable key set. If both replicas restart while hermit is away **and** the database has no
  `token_keys` row or is away too, no egress pod becomes ready: the VIP has no target and every sandbox loses magpie,
  hermit, the model and the internet together, so running turns stop until hermit or the database is back (aviary
  platform 3.2 wants readiness independent of other services; a gateway that cannot verify a token cannot serve).
- The hermit-outage check in CI is accept-and-reset only, not connection refused (`run.sh`, the suite's fakes keep
  :80); the conformance suites are built from source at `chat/aviary.ref`, not run from the published
  `ghcr.io/de0ch/aviary-testing` image (aviary WP-28).
- The old relay listener (staff's own Claude Code on the model route, `EGRESS_RELAY_PORT`, table `relay_tokens`) is
  not in this egress: awaiting Deyao's decision (aviary refactor DESIGN 10, "Open: the relay").

**Configuration** `/etc/egress/egress.json` (`chat/src/egress/config.ts`): `product` (`name`, `locale` for the budget
message), `models`, `defaultModel`, `smallModel`, `titleModel`, `titleReasoning`, `contextTokens`, `routes` (exactly
`model` and `tools`: `{name, upstream, model?, inject, allowPaths?, timeoutSeconds?}`), `selfHosts`,
`generatedAllowlist`, `defaultAllowedHosts`, `denyHosts`, `blockCidrs`, `maxUploadBytes`, `cacheGiB`, `budget`.
**Env:** `EGRESS_DATABASE_URL`, `EGRESS_TOKEN_MAGPIE`, `EGRESS_TOKEN_HERMIT`, `EGRESS_ACCEPT_PIGEON`,
`EGRESS_ACCEPT_HERMIT`, `EGRESS_ACCEPT_ADMIN` and every variable a route's `inject` names (`OPENROUTER_API_KEY`,
`EGRESS_TOKEN_TOOLS`); a missing one is a start failure. **CI:** chat.yml's `conformance` job runs aviary's
`gateway` and `model` suites against the image under test (`chat/test/conformance/run.sh`, the aviary commit in
`chat/aviary.ref`) on the chat image built once by the `chat-image` job; `images` pushes that same image (its id
checked), only when the tests and both suites pass.

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

### `tools` (`node src/main.ts tools`; ports 8080 `POST /mcp`, 9090 health; MCP Streamable HTTP, stateless, JSON responses)

aviary platform spec 17.2 tools. The MCP server Claude Code in a sandbox sees is named `tools` (tool names
`mcp__tools__*`); it reaches it as `http://tools/mcp` through the gateway (egress's route `tools`).

- **Caller: the gateway only.** `Authorization: Bearer <pair token>`, one of `TOOLS_ACCEPT_GATEWAY` (comma-separated,
  current first; two during a rotation; compared by SHA-256, constant-time). Anything else → 401 `unauthenticated`.
  The gateway names the sandbox it verified: `Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation`,
  `Aviary-Sandbox-Person` (the owner's lowercased email) and `Aviary-Request`; without the three sandbox headers → 400
  `bad_request`.
- **Owner = `Aviary-Sandbox-Person`.** It is `${USER_EMAIL}` in upstream headers (so yyc-mcp gets
  `X-YYC-User: <email>` and checks it against the catalogue) and `tool_calls.user_email`.
- **Access checks**, each refused with 403 `not_allowed`: the person must be allowed by the staff catalogue
  (`/etc/tools/catalogue/catalogue.json`, ConfigMap `catalogue`; hermit's rules: exact address or `@domain`, re-read
  every 30 s, a file that fails validation is refused and the last good one kept); and hermit's liveness answer for
  the sandbox (`GET http://hermit/v1/sandbox-tokens/{sandboxId}`, bearer `TOOLS_TOKEN_HERMIT`, timeout 1 s) must say
  `live`, state `starting` or `ready` (`paused`, `releasing`, `gone` and a 404 are refused), and the conversation and
  person the gateway named. The answer is cached 30 s and refreshed in the background; while hermit is unreachable,
  slow or failing, the last answer is served whatever its age, and a sandbox with no answer yet is served on the
  gateway's verification. So a hermit outage never stops a running turn's tool calls; a release takes effect within
  30 s once hermit answers. No database is read for any of this.
- Every call → a `tool_calls` row in tools' own database (`TOOLS_DATABASE_URL`, migrations in
  `chat/src/tools/db.ts` under tools' own advisory lock): conversation, sandbox, person, request, tool, args, ok, ms,
  error, cost. Written without waiting: while the database is away a row is dropped and counted
  (`tools_tool_calls_dropped_total`), the call answers as usual.
- **Read-only tools only.** Nothing here sends, posts, writes or changes anything outside, and nothing asks for or
  waits on a confirmation. Tools (names lowercase, `snake_case`; annotations set `readOnlyHint`): `web_search`,
  `web_fetch`, `social_search` (TikHub), the AnyAPI tools (below), plus the **listed** tools of each configured upstream
  MCP server, exposed as `<prefix>_<tool>` (YYC: prefix `yyc`, the yyc-mcp Worker, whose `wisers_*` tools are YYC's WiseSearch, service token +
  `X-YYC-User: <owner email>`).
- **Upstream tools: an explicit read-only allowlist, default deny.** `upstreams[].tools` (tools.json) names the upstream's
  tools (their own names, without the prefix) that may be offered: search/get/list-style tools only. A tool not on
  the list is hidden (a new upstream tool stays hidden until someone adds it), and so is a listed tool the upstream
  marks `readOnlyHint: false` or `destructiveHint: true`. An upstream without a `tools` list offers nothing. Hidden
  tools are not in `tools/list` and `tools/call` answers "unknown tool" for them; each refresh logs the hidden names
  (`upstream tools … hidden`). Anything that sends mail, posts or mutates (e.g. a `send_mail`) must never be listed.
- **AnyAPI** (`tools.anyapi`: `{keyEnv, baseUrl?, denySkus?}`; getanyapi.com, about 470 read-only scraping and data
  APIs behind one key: Facebook, LinkedIn, Instagram, TikTok, YouTube, X, Reddit, Google, shops, jobs, company data,
  any page as Markdown): `anyapi_search` (its catalog search, free), `anyapi_describe` (one API's input/output schema
  and price, free), `anyapi_run` (POST /v1/run/<id> with the model's input, optionally shaped by `fields`/`max_items`/
  `summary`/`jq`; the response's `costUsd` goes into `tool_calls.cost_usd`; up to 120 s), `anyapi_result` (re-read or
  reshape a run's `resultId` free for about 15 minutes). Built in rather than relayed from AnyAPI's own MCP server:
  that server marks `run_api` destructive (it spends money), which the upstream rule above hides, and its
  `report_bug`/`send_feedback` send things to AnyAPI. Every catalog API only reads; `denySkus` keeps an id out
  anyway. HTTP 402 / `insufficient_balance` is answered "AnyAPI's balance has run out … the admin has to add funds".
- A built-in tool whose provider is not configured is left out of `tools/list` (`web_search` needs `exa`,
  `social_search` `tikhub`, the `anyapi_*` tools `anyapi`; `web_fetch`
  without Exa reads pages directly).
  `web_fetch`'s direct path refuses non-public addresses (checked after DNS, connecting to the checked address) and
  ports other than 80/443/8080/8443. With `capture` (`{url, tokenEnv, name}`: the instance's own page capture,
  POST `{url}` with that bearer → `kind` cache / capture / none), `web_fetch` asks it first: a `cache` or `capture`
  answer is returned with its source on the first line ("Source: YYC's stored copy (captured …)" / "YYC's capture,
  read just now") and `structuredContent.via` `own-cache` / `own-capture`; `none`, an error or `original: true` reads
  the page as before (Exa, then directly). YYC: its news monitor (news-stream webfetch.py). `tikhub.platforms` (optional) limits `social_search` to a subset of
  `xiaohongshu, douyin, weibo, x, threads, youtube, instagram, reddit`. Upstream tool lists are read at startup and
  every 10 minutes as the upstream's `listAs` (`${USER_EMAIL}` = that address while listing, the sandbox's owner on
  calls); a failed read keeps the last list.
- Config `/etc/tools/tools.json` (ConfigMap `tools-config`), the old shared config's `tools` section plus `listAs`:

  ```json
  {"exa": {"keyEnv": "EXA_API_KEY"}, "tikhub": {"keyEnv": "TIKHUB_API", "platforms": ["xiaohongshu", "douyin", "weibo"]},
   "anyapi": {"keyEnv": "ANYAPI_API_KEY"}, "capture": {"url": "http://capture.example/fetch", "tokenEnv": "WEB_FETCH_TOKEN", "name": "YYC"},
   "upstreams": [{"prefix": "yyc", "url": "http://mcp.example/mcp", "listAs": "<an allowed address>",
                  "headers": {"Authorization": "Bearer ${YYC_MCP_TOKEN}", "X-YYC-User": "${USER_EMAIL}"},
                  "tools": ["posts_search", "post_get", "streams_list"]}]}
  ```
  Every variable it names must be set at start (a start failure names it, never its value).
- Env, all required: `TOOLS_DATABASE_URL`, `TOOLS_ACCEPT_GATEWAY`, `TOOLS_TOKEN_HERMIT`, plus the key variables the
  config names.
- Health on 9090: `/healthz`, `/readyz` (503 until migrations are applied, the catalogue has been read once and the
  first upstream refresh has run, and while draining; never for a database or hermit outage later), `/metrics`
  (`tools_requests_total{status}`, `tools_calls_total{ok}`, `tools_liveness_lookup_failures_total`,
  `tools_liveness_stale_served_total`, `tools_catalogue_refused_total`, `tools_tool_calls_dropped_total`,
  `tools_drain_cut_total`), `/version`.
- Drain (SIGTERM after the 10 s preStop): `/readyz` 503, no new connections, a request on a kept-alive connection is
  served with `Connection: close`, in-flight calls finish up to 150 s (an upstream call's own timeout is 120 s), what
  is left is cut and counted, exit 0 with log `drained`. `terminationGracePeriodSeconds` 180. The bound is code, not
  configuration.

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

**The old stack's egress and tools are pinned (aviary refactor, until A6).** From WP-25 on, this repo's egress is
aviary's sandbox gateway (section "`egress`: the sandbox gateway"), and from WP-26 on its tools serve that gateway
(section "`tools`"); this base can run neither. `deploy/chat/egress.yaml` and `deploy/chat/tools.yaml` therefore name
`ghcr.io/de0ch/jarvis3-chat:f91907d349e6` themselves (the last image of the old egress, whose allowlist is the
refresh of 2026-11-02, and of the old tools), and an instance's `images:` entry for `jarvis3-chat` does not touch
them. web still starts from `JARVIS_ROLE` with no argument, so bumping the base and the image tag ships web fixes to
the old stack as before; its egress and tools get no more changes, the weekly allowlist refresh included. The new
stack (YYC's own manifests, `args: [egress]`, `args: [tools]`) takes the new ones. All of it goes in A6 with
`deploy/chat/**` and `JARVIS_ROLE`.

A new version = bump the base `ref` and the image tags (chat image in `images:`, the sandbox's in `config.json`
`sandbox.image`) to a commit whose `chat` workflow pushed images, and push; Flux rolls the Deployments one pod at a
time. A new sandbox image reaches new sandboxes only (each host fetches it once); running ones keep theirs.

## Budget (`budget` in egress.json, optional; absent = no limit; `chat/src/egress/budget.ts`)

One limit for everyone, admins included. Spend = `sum(egress_log.cost_usd)` of kind `model` (every model call Claude
Code makes, its small/fast-model calls too) with `at >= since`. Spent ≥ `limitUsd` = used up: egress refuses every
model call with 402 `{"type":"error","error":{"type":"budget_exceeded","message":"<message>"}}` (`deny:budget`). A
call already in flight when the limit is crossed completes, so the spend can end above the limit by about one model
call per running conversation. Message (`product.locale`): zh-HK「<ownerName> 設定了 <limit> 美元的使用上限，目前已用完。」,
English "<ownerName> has set a USD <limit> budget limit and it has now been used up." (no `ownerName`: 管理員 / The
administrator). There is no automatic reset. To reset or raise: edit `budget` in egress.json (`since` = now resets
the spend to 0; a bigger `limitUsd` raises it) and roll out egress. Check:
`select sum(cost_usd) from egress_log where kind = 'model' and at >= '<since>'`.

## Config (`/etc/jarvis/config.json`, from the instance)

```json
{
  "product": {"name": "YYC AI", "locale": "zh-HK"},
  "publicUrl": "https://ai.yyc-internal.com",
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
  //   "egressUrl": "wss://egress.ai.yyc-internal.com/wire", "egressHeaders": {}, "apiKeyEnv": "E2B_API_KEY", "instance": …}}
  // the sandbox allowlist ("The sandbox allowlist"): the generated lists (generatedAllowlist, default true) plus
  // defaultAllowedHosts; denyHosts wins over everything; maxUploadBytes per tunnel/request (default 8 MiB)
  "egress": {"defaultAllowedHosts": [], "denyHosts": [], "generatedAllowlist": true, "maxUploadBytes": 8388608,
             "routes": [{"name": "claude", "upstream": "https://openrouter.ai/api", "model": true,
                         "inject": {"Authorization": "Bearer ${OPENROUTER_API_KEY}"}},
                        {"name": "tools", "upstream": "http://tools.jarvis.svc.cluster.local:8080", "keepAuth": true,
                         "inject": {}, "allowPaths": ["/mcp"]}], "cacheGiB": 5},
  "tools": {"exa": {"keyEnv": "EXA_API_KEY"}, "tikhub": {"keyEnv": "TIKHUB_API", "platforms": ["xiaohongshu", "douyin", "weibo", "x", "threads", "youtube", "instagram", "reddit"]},
            "upstreams": [{"prefix": "data", "url": "https://yyc-mcp.yyc-news.workers.dev/mcp",
                           "headers": {"Authorization": "Bearer ${YYC_MCP_TOKEN}", "X-YYC-User": "${USER_EMAIL}"},
                           // read-only tools to offer (default deny; "tools" above): the upstream's own names
                           "tools": ["posts_search", "post_get", "streams_list"]}]},
  "budget": {"limitUsd": 30, "since": "2026-10-21T12:00:00Z", "ownerName": "Deyao"}
}
```

**Who and which models:** `users` (lower-cased) is who may sign in (the UI's router checks it; web checks
`/internal/cc/sandbox`'s owner against it); `admins` may open the operator page (router and web check it) and the
first admin is the identity the old stack's tools (the pinned image) list upstream tools as. `access` is the Cloudflare Access app the UI's router verifies. `models`/`defaultModel`/`titleModel`: egress's
model routes refuse any other id (400, `deny:model`); `defaultModel` is Claude Code's model in the sandbox
(`ANTHROPIC_MODEL`), `titleModel` its small/fast model. Thinking: a model's `reasoning` (`none`, `low`, `medium`,
`high`, `max`) is set by egress on that model's requests that carry tools (Claude Code's turns), `titleReasoning` on the
title model's tool-less ones (titles, Claude Code's quick calls); absent = the model's own default. On OpenRouter's
Messages API only `thinking: {type: "disabled"}` turns thinking off (`reasoning.effort: "none"` is ignored), so `none`
is sent that way and any other level as `reasoning.effort`. A model's `contextWindow` (tokens) reaches Claude Code as
`CLAUDE_CODE_MAX_CONTEXT_TOKENS`: Claude Code takes a model it doesn't recognise (any non-Anthropic one) as 200k and
auto-compacts it at ~170k, whatever the model's real window.

Secrets come from the environment (`DATABASE_URL`, `JARVIS_CC_TOKEN` (web), `OPENROUTER_API_KEY` (egress only), tool
keys (tools only); provider "vm", web only: `SBX_CA_CERT`, `SBX_CLIENT_CERT`, `SBX_CLIENT_KEY`, `SBX_HOST_CERT`,
`SBX_HOST_KEY` (PEMs), the scaler's least-privilege AWS key `SBX_AWS_ACCESS_KEY_ID`, `SBX_AWS_SECRET_ACCESS_KEY`, and
`JARVIS_JUICEFS_META_URL`/`JARVIS_JUICEFS_META_PASSWORD` for the burst hosts' user data). Each role gets only the
Secret it needs.

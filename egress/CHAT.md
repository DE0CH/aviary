# Jarvis 3 chat runtime

Status: being built 2026-10-20 for the YYC instance (YYC AI, `ai.yyc.deyaochen.com`). Agreed design: Deyao,
2026-10-20. This is a second runtime next to the task runtime in `server/` (OpenCode in Fly machines): a staff web
chat whose agent loop runs **on the server**, with bash sent to a per-conversation sandbox. The code is in `chat/`,
its Kustomize base in `deploy/chat/`. An instance pins a tag of this repo and supplies its config and secrets.

Staff never see the name "Jarvis 3": the product name, logo and wording come from the instance config.

**What it is (Deyao, 2026-10-21): a read-only data-analysis tool, not an assistant that acts.** It searches, reads
and analyses (news, social posts, the instance's own data, uploaded files, a sandbox for code and data work); it never
sends, posts or changes anything outside the chat. So there is nothing to confirm and nobody is ever asked anything:
every tool is read-only (outward actions such as sending mail are not offered at all), and the sandbox's internet
access is decided automatically by an allowlist with no manual override ("either allow automatically or deny with
no manual override"). There are no confirmation cards, no per-conversation site grants and no
`request_network_access` tool (all removed in schema 6).

## Components (one image `ghcr.io/de0ch/jarvis3-chat`, role chosen by `JARVIS_ROLE`)

| Role | What | Replicas |
|---|---|---|
| `web` | Web UI, API, SSE streams, turn runner (spawns `claw`), sandbox manager, files, janitor | 2, rolling, drained |
| `egress` | The only way out of a sandbox; also the model gateway for claw. Token identity, the automatic sandbox allowlist with threat-intel DNS, credential injection, logs every call, package cache | 2, rolling |
| `tools` | MCP server (Streamable HTTP, JSON responses): news search, web search/fetch, upstream MCP tools; read-only tools only | 2, rolling |

Plus the sandbox image `ghcr.io/de0ch/jarvis3-sandbox` (runs `sandbox-agent`): run by our own sandbox hosts
(`sandbox.provider: "vm"`, the YYC instance since 2026-10-21: gVisor on a Hetzner hub + AWS burst hosts with live
migration), built as an E2B template for hosted sandboxes (`"e2b"`), or started as gVisor pods
(`runtimeClassName: gvisor`) in namespace `jarvis-sandbox` (`"kubernetes"`: tests, local runs, a self-hosted
cluster). See "Sandbox providers".

```
browser ──Access──▶ web ──spawn per turn──▶ claw (cwd = conversation workdir, file tools confined there)
   ▲  SSE (events from Postgres)            │ model calls ─▶ egress /x/model ─▶ OpenRouter (real key injected)
   │                                        │ MCP (stdio bridge) ─▶ tools ─▶ Exa / TikHub / Wisers relay / yyc-mcp
   │                                        │ bash ─▶ jarvis-bash ─sync files + exec─▶ sandbox pod (gVisor)
   └── events (text, tool steps) ◀──────────┘                                    │ all egress
                                                                                 ▼
                                                 egress (token → sandbox allowlist → threat-intel DNS → public IP)
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

- **Made at the first bash command, never ahead of it.** jarvis-bash asks the turn runner for the conversation's
  sandbox (`POST /sandbox` on the replica's unix socket, with the turn's token); the sandbox manager makes one if
  there is none (a sandbox starts in well under a second once a host is up), and the workspace files are pushed into
  it before the command runs. A turn that never runs bash never gets a sandbox; typing makes none either. There is at
  most one live sandbox per conversation (unique index `sandboxes_one_live`).
- **Typing only readies a host:** the UI calls `POST /api/typing` (debounced, at most every 30 s, with the open
  conversation's id if any). If that conversation has a live sandbox it is kept active; otherwise it is a signal
  (kv `sandbox_demand:<email>`, counted for 2 minutes) on which the vm scaler starts an AWS host at once when no host
  has a free slot (docs/SANDBOX-VM.md). No conversation row is made for typing.
- **Kept while active:** installs, caches and background processes carry over between turns.
- **Released** after `sandbox.idleMinutes` (default 30) with no turn and no typing, and always at
  `sandbox.maxLifetimeHours` (default 8). Before deleting the pod the janitor pulls the sandbox's changed files and
  saves them (files are also synced after every bash command, so this only catches what background processes wrote).
- **Also released:** at once when the conversation is deleted (`DELETE`; the janitor catches a replica that died
  first), and by the janitor when its conversation never had a turn 5 minutes after it was made (a leftover; nothing
  makes such a sandbox any more). The janitor also removes sandboxes a provider lists without a live row, and marks
  rows whose sandbox the provider no longer has.
- **Reset notice:** when the first bash command of a conversation lands in a fresh sandbox and the conversation had
  an earlier one, that command's output starts with a line: `[System note: the sandbox was reset: files in the
  workspace are restored; installed packages and background processes from before are gone.]`
- **No secrets in a sandbox.** Its env has only `HTTP_PROXY`/`HTTPS_PROXY`/`http_proxy`/`https_proxy`
  `=http://sbx:<egress token>@egress.<ns>.svc.cluster.local:3128`, `NO_PROXY=` (empty), the pip/npm mirror settings
  and its exec token. NetworkPolicy: egress only to the `egress` pods (3128) and cluster DNS; ingress only from `web`
  pods on 7070.
- Limits per sandbox: `sandbox.cpu` (default "1"), `sandbox.memory` (default "1Gi"), ephemeral storage 5Gi;
  at most `sandbox.maxActive` (default 6) live sandboxes per instance; over the limit the least recently active idle
  one is released first; if none is idle, the bash command waits up to 60 s, then reports
  "the sandbox pool is full, try again in a minute".

## Sandbox providers (`sandbox.provider`)

The sandbox manager (`chat/src/web/sandbox.ts`) owns the lifecycle above and the `sandboxes` rows; a provider
(`chat/src/web/provider.ts`) only creates (`create` → its name for the sandbox, stored in `sandboxes.pod`), waits for
the agent (`waitReady` → `{url, headers}`, stored in `sandboxes.agent`), keeps alive, destroys and lists. jarvis-bash
gets the URL and headers (`JARVIS_SANDBOX_URL`, `JARVIS_SANDBOX_HEADERS`), so it works the same with either.

- **`vm`** (`sandbox-vm.ts`, `vm-clients.ts`; host agent `chat/sandboxd`; the YYC instance since 2026-10-21): gVisor
  sandboxes (the DE0CH/gvisor fork) on our own hosts: one always-on Hetzner hub plus AWS EC2 burst hosts (at most
  `aws.maxHosts`), live-migrated between them (pre-copy) so the scaler can drain AWS hosts back onto the hub and
  terminate them. Full design, operations and measurements: docs/SANDBOX-VM.md.
  - **Create:** the controller (web) places the sandbox (the hub while it has room, else the fullest AWS host with
    room; none: the scaler starts one, `create` waits up to ~170 s, or answers "all sandbox hosts are full" at the
    cap) and gives it a fixed /32 from `sandbox.vm.sandboxCidr` (`sandboxes.ip`; `sandboxes.host` = where it runs
    now). sandboxd on that host (`POST /v1/sandboxes`, mutual TLS) starts it with the env (proxy and mirrors at
    `http://sbx:<egress token>@<egressVip>:3128`, `JARVIS_EXEC_TOKEN`). The sandbox's first process is
    `sbx-init.mjs` (root inside gVisor only, to start dockerd on the first `docker` command); the agent runs as uid
    1000 with no capabilities and no_new_privs.
  - **Ready:** `http://<ip>:7070/healthz` (no provider headers); the URL never changes, also across migrations.
  - **keepAlive:** a no-op; the janitor releases (with the final save).
  - **Egress:** sandboxes reach egress at `sandbox.vm.egressVip`, which the hub translates (DNAT only) to an egress
    pod: egress sees the sandbox's own address, and egress only accepts a vm sandbox's token from that address. Each
    egress pod announces `POD_IP` in `kv egress_pod:<pod>`; the scaler hands the live ones to the hub.
  - **Janitor:** `list()` = every host's `GET /v1/sandboxes` (an unreachable host's rows count as present; the scaler
    decides when a host is lost). `sandbox.vm.legacyProvider: "kubernetes"` during a switch: the previous provider's
    sandboxes (rows without a host) stay listed and are removed through it once released.
  - **Scaler** (web, Postgres leader lock 4242012, every 5 s): heartbeats (a host silent 30 s, the hub 120 s, loses
    its sandboxes: rows `gone`, "host lost", reset note on the next turn), the hub's WireGuard peers and egress
    targets, AWS hosts up (spare slots) and down (drain-back, one idle sandbox at a time, never during a turn; a move
    across clouds only when the sandbox is idle and aborted, unchanged, if pre-copy wouldn't converge within
    `migration.maxDowntimeMs`), empty AWS hosts terminated. Operator knobs live in table `vm_hosts`
    (`max_sandboxes`, `status` `retiring`).
- **`kubernetes`** (`sandbox-k8s.ts`): a gVisor pod per sandbox in `sandbox.namespace`, agent at `http://<pod IP>:7070`,
  the env (proxy, mirrors, `JARVIS_EXEC_TOKEN`) in the pod spec, the proxy `sandbox.egressProxyUrl` (default
  `http://egress.<ns>.svc.cluster.local:3128`). No provider timeout: `keepAlive` is a no-op. `create` retries for up
  to a minute while a namespace ResourceQuota still counts a pod the pool just released (it terminates for a few
  seconds). The YYC instance runs these pods in its own cluster (DE0CH/YYC `yyc-ai/deploy/sandbox.yaml`: RuntimeClass on
  labelled nodes, a PriorityClass below its jobs that never preempts, quota, the web service account's Role, policies).
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
  - **Lifetime:** E2B kills a sandbox at its timeout. On every typing signal, every turn and every minute of a running turn
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
  error text, usage jsonb, cost_usd numeric,
  presented jsonb);  -- migration 7: the files the turn shows, [{path, caption?, sha256, size}]; null = before present_files
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
  expires_at timestamptz not null, released_at timestamptz, release_reason text,
  agent jsonb,                    -- {url, headers}: how to reach its agent (migration 2)
  host text, ip text);            -- provider vm: the host it runs on now, its fixed /32 (migration 3)
create table vm_hosts (name text primary key, kind text,   -- 'hub' | 'aws'   (provider vm, migration 3)
  url text, wg_ip text, wg_public_key text, instance_id text, private_ip text,
  status text,                    -- 'booting' | 'active' | 'draining' | 'retiring' | 'gone'
  max_sandboxes int,              -- operator's cap (null: by memory/CPU)
  mem_total bigint, mem_reserve bigint, cpus int, info jsonb, created_at, last_seen_at, empty_since, gone_at, gone_reason);
-- allowed_hosts (conversation_id, host, added_by, created_at): unused since migration 6 (emptied; the sandbox
-- allowlist is automatic); kept so an older replica still running during that rollout can read it
create table egress_log (id bigserial primary key, at timestamptz not null default now(), conversation_id uuid,
  sandbox_id uuid, turn_id uuid, kind text not null,   -- 'connect' | 'http' | 'mirror' | 'model' | 'inject'
  method text, host text not null, port int, path text, status int, decision text not null,  -- 'allow' | 'deny:<why>'
  bytes_up bigint, bytes_down bigint, ms int, cost_usd numeric);
create table tool_calls (id bigserial primary key, at timestamptz not null default now(), turn_id uuid,
  conversation_id uuid, user_email text, tool text not null, args jsonb, ok boolean, ms int, error text, cost_usd numeric);
-- confirmations (id, conversation_id, turn_id, kind, summary, detail, status, …): unused since migration 6 (nothing
-- asks the user anything; pending rows were expired); old rows only
create table kv (key text primary key, value jsonb not null);
-- the admin monitor (migration 5): sandbox starts (kind 'sandbox_start': ok, ms = allocation latency from the first
-- request incl. pool wait, detail {reason, error, provider, startMs}); kept 30 days by the janitor
create table ops_events (id bigserial primary key, at timestamptz not null default now(), kind text not null,
  ok boolean, ms int, sandbox_id uuid, conversation_id uuid, host text, detail jsonb not null default '{}');
```

Notifications: `pg_notify('conv_<uuid without dashes>', '<event id>')` after inserting events. `turns.status`
`waiting` (a turn held by a confirmation) no longer occurs; migration 6 set any such turn back to `running`.

### Tokens

- **Turn token** (32 random bytes, base64url), made by `web` per turn, stored as `turns.token_hash`, valid while the
  turn is `running`/`waiting`. claw gets it as `ANTHROPIC_AUTH_TOKEN` (egress `/x/model`) and the MCP bridge sends it
  as `Authorization: Bearer` to `tools`.
- **Egress token**, per sandbox, stored as `sandboxes.egress_token_hash`; valid while the sandbox is not `gone`.
- **Exec token**, per sandbox: `web` → `sandbox-agent` only (`Authorization: Bearer`).

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
   (`{name, upstream, inject: {header: value-from-env-var}, auth: "turn"|"sandbox", allowPaths?}`). The YYC instance
   has one: `model` → `https://openrouter.ai/api`, auth `turn` (`Authorization: Bearer <turn token>` or
   `x-api-key`), injects `Authorization: Bearer $OPENROUTER_API_KEY`, forces zero data retention into every request
   body (`provider.zdr: true`, `provider.data_collection: "deny"`, set over whatever the caller sent; a body that is
   not a JSON object is refused with 400, `deny:body`; no config turns this off: `forceZdr` in
   `chat/src/egress/proxy.ts`), streams responses through, logs `cost_usd` from
   the response's `usage.cost` (SSE: the final `message_delta` usage) into `egress_log` (kind `model`).
   The caller's `Authorization`, `x-api-key` and `Cookie` never go upstream; egress asks upstream for an
   uncompressed body (`accept-encoding: identity`) so it can read the cost.
   The `model` route also refuses, before anything goes upstream: a body whose `model` is not one of config `models`
   or `titleModel` (400 `invalid_request_error`, `deny:model`), and every call once the shared budget is used up
   (402 `{"type":"error","error":{"type":"budget_exceeded","message":"<budget message>"}}`, `deny:budget`; a model
   call's `egress_log` row is written as soon as it ends and the budget is re-read at most every 0.5 s; see "Budget").
4. **Env:** `DATABASE_URL`, `JARVIS_CONFIG`, `JARVIS_CACHE_DIR` (default `/var/cache/jarvis-egress`, an emptyDir),
   `EGRESS_SELF_HOSTS`, `POD_NAMESPACE`, `EGRESS_BLOCK_CIDRS` (extra refused ranges, comma list: the node's own
   public /64 on YYC), `EGRESS_DRAIN_SECONDS` (default 120), and every variable a route's `inject` names
   (`OPENROUTER_API_KEY`). Identity lookups are cached for 5 s. At start it logs `egress allowlist` with the lists'
   version and sizes.

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
- **The agent is told:** the runtime appends a note to the instance's system prompt on every turn
  (`SANDBOX_NETWORK_NOTE`, `chat/src/web/turns.ts`): the sandbox reaches an automatic allowlist only, nobody can enable
  a site from the chat, and web pages are read with `web_fetch`/`web_search` (server-side, any public site).
   On SIGTERM: `/healthz` turns 503, the listener closes, open requests and tunnels get up to the drain time,
   the log is flushed.

### `tools` (port 8080, `POST /mcp`, MCP Streamable HTTP, stateless, JSON responses)

- Auth: `Authorization: Bearer <turn token>` → turn → conversation → owner email. Invalid → 401.
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
  every 10 minutes as the first admin (`${USER_EMAIL}` = that admin while listing, the turn's owner on calls).
- Env: `PORT` (8080), `JARVIS_DRAIN_MS` (150 s: an upstream call, up to 2 minutes, finishes before the pod stops;
  `terminationGracePeriodSeconds` 180), plus `DATABASE_URL` and the key env vars the config names.
- **`jarvis-mcp-bridge`** (claw's stdio MCP server `tools`, in the image at `src/bin/`): reads JSON-RPC framed
  with `Content-Length` headers (claw's MCP client) or one message per line, answers in the same framing, POSTs each
  message to `$JARVIS_TOOLS_URL` (`http://tools.<ns>.svc.cluster.local:8080/mcp`) with
  `Authorization: Bearer $JARVIS_TURN_TOKEN`; retries a 503 (a draining replica) up to 3 times.

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
stderr). The helper (in the `web` image) gets `JARVIS_SANDBOX_SOCKET` (the replica's unix socket, mode 600 in the work
root), `JARVIS_TURN_TOKEN` and `JARVIS_SYNC_STATE` (a JSON file outside the workdir with the last-synced manifest and
the `/changes` cursor) from the turn runner. Each call first asks the socket for the sandbox (`POST /sandbox`, bearer
= the turn token; only a turn running on that replica is accepted) → `{id, url, token, headers, reset}` (made now if
the conversation has none: `sandbox` events `starting`/`ready`, and `reset` → the reset notice above the output) or
`{unavailable: <reason>}`; then: push files that changed in the workdir since the manifest (`PUT /files`),
`POST /exec`, pull `/changes` into the workdir and update the manifest. Without a sandbox the helper fails with a
plain sentence (`The sandbox is not available for this message (<reason>). Try again in a minute.`, the reason the
pool-full text or "it could not be started"), never an internal name, since the model passes it on to staff. A
different sandbox id than the one in the state file (a new sandbox) resets the manifest and cursor, so the whole
workdir is pushed again. A full `/changes`
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
| `CLAW_IMAGES=1` | images reach the model: `read_file` on png/jpeg/webp/gif returns the image (heic/svg/bmp/tiff and a PDF page, `page`, converted to PNG), at most 1568 px on the long edge and 5 MB, at most 20 per run (then a text note); the session stores a reference (workdir path, sha256, media type), the bytes are read from the workdir for each request; `tool_result` events carry `images: [{path, sha256, media_type, width, height, page?}]` |
| `CLAW_PROMPT_IMAGES` | JSON array of workdir paths attached to the prompt's user message as images (same rules) |

### Turn runner (in `web`)

1. `POST /api/conversations/:id/messages` → insert the user message and a `turns` row (`queued`), answer 202 with ids.
   One running turn per conversation (409 otherwise). The turn's model is the conversation's while config `models`
   still offers it, else `defaultModel` (the conversation is moved to it: e.g. Opus conversations continue on Sonnet
   after a switch to Sonnet only).
   **Budget:** when the shared budget is used up the turn ends at once (`error` = the budget message, events
   `turn_started` + `turn_finished {code: "budget", limitUsd, ownerName}`): no sandbox, no claw, no model call. A
   running turn whose model call egress refuses for the budget (claw: `error {message: "api returned 402 …
   (budget_exceeded): …"}`) is stored with that `error` event rewritten to `{code: "budget", message, limitUsd,
   ownerName}` and ends `error` with the budget message; its text so far stays.
2. The receiving replica runs it: reconcile the local workdir `/var/lib/jarvis/work/<conv>` with `files` (download
   from S3 what differs), write `claw_sessions.content` to `<sessions dir>/<conv>.jsonl`, ensure the sandbox, then
   spawn `claw --model <model> --permission-mode danger-full-access --allowedTools <list> --output-format json
   prompt <text>` with `CLAW_EVENT_STREAM=1`, `CLAW_RESUME_SESSION=<path>` (when a session exists), and the env above.
   The MCP config (in `CLAW_CONFIG_DIR/settings.json`) lists one stdio server `tools` = `jarvis-mcp-bridge` (stdio ↔
   HTTP, token from env, `JARVIS_WORKDIR` = the workdir). Also `CLAW_IMAGES=1` and, when the message has image
   attachments (png, jpeg, gif, webp, heic, heif, bmp, tiff, svg; at most 20), `CLAW_PROMPT_IMAGES` (see "Presented
   files and images"). The system prompt file = the instance's prompt + `SANDBOX_NETWORK_NOTE` + `FILES_NOTE`.
3. Each NDJSON event → `events` (+ `pg_notify`). Text deltas are batched (≤ 250 ms) into one `text_delta` event.
4. End: save the session file to `claw_sessions`, upload changed workdir files to S3 (`files/<conv>/<sha256>`) and
   upsert `files` (source `agent`), settle the presented files (each at the version the turn left, gone ones dropped;
   a new `files_presented` event when that changed the list), insert the assistant message (final text), set the
   turn `done` with usage and cost. Title: after the first turn, a short title from the model (Haiku-class, via egress) if the user did not set one.
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
   restarted"), releases idle/expired sandboxes, ends stuck starts, deletes
   sandbox pods without a live row and marks rows whose pod vanished.

### Presented files and images (Deyao, 2026-10-21)

- **Staff see only what the agent presents.** Under an answer the UI shows download cards for exactly the files of
  that turn's `presented` list, in its order (png/jpeg/gif/webp with an inline preview, a PDF with its first page),
  each linked at its version (`?v=<sha256>`, so a later turn changing the path doesn't change an earlier card). There
  is no file browser and no automatic list of changed files; staff uploads show in their own messages (images as
  thumbnails). The workdir is stored and synced as before; the agent keeps using every file.
- **`present_files({files: [{path, caption?}]})`** (tools role, read-only, `chat/src/tools/present.ts`): 1–20 paths
  relative to the workdir (`/work/x` and `./x` accepted). The tools role can't see the workdir, so `jarvis-mcp-bridge`
  (on the replica running the turn, `JARVIS_WORKDIR`) checks each path before forwarding and puts what it found in the
  call's `params._meta["jarvis/files"]` (`{path, size, sha256}` or `{error}`): a regular file inside the workdir, no
  symlink, not under an excluded folder (`node_modules/`, `.jobs/`, …), at most 50 MB. One bad path refuses the whole
  call with every problem named. A good call merges into `turns.presented` (row lock) and appends `files_presented`.
  Merging (`chat/src/common/presented.ts`): a path given again replaces its entry at the new place, and the same
  contents under another path (a copy) are shown once, as the latest; at the end of the turn each entry takes the
  version the turn left (the latest wins). The agent is told (`FILES_NOTE`): "When you finish, call present_files with
  the deliverables only (reports, charts, data the user asked for) — never scripts, fonts or intermediate files."
- **The duplicate PDF (2026-10-21):** the old UI listed every agent file changed during a turn by its file name; the
  agent built `build/<name>.pdf` and then `cp` it to the workdir root, so two paths with the same name and contents
  showed as two identical cards. Fixed both by showing only presented files and by the copy rule above.
- **Old conversations:** a turn with `presented` null (run before migration 7, or by an older replica during the
  rollout) shows, as before, the agent files whose last change falls between its start and 15 s after its end
  (`legacyPresented` in `app.ts`, computed when the conversation is read), with copies shown once. Nothing is lost:
  reports made before stay downloadable from their answer. A new turn that presents nothing shows no cards.
- **Images to the model** (claw `CLAW_IMAGES`, above): `read_file` on an image or a PDF page shows it to the model;
  staff-uploaded images in a message go with it (`CLAW_PROMPT_IMAGES`). Sandbox-made images need nothing extra: files
  written under `/work` are in the workdir after each bash command. The tool step of a viewed image shows a thumbnail:
  the runner puts that file's bytes in the bucket under its sha256 as soon as the `tool_result` arrives
  (`?v=<sha>&preview=1` then loads mid-turn). OpenRouter passes Anthropic `image` blocks (user and `tool_result`)
  through with ZDR forced (checked 2026-10-21, `anthropic/claude-sonnet-5.5`). The web image has `poppler-utils` and
  `libheif-examples` for the conversions.

### Events (what the UI renders; `GET …/stream` and `turns[].events`)

Each is `{id, turnId, seq, type, data, createdAt}`; the list of types is in "Public API" below. What `web` itself adds:
`turn_started {turnId, model}`; `sandbox {status: starting|ready|reset|unavailable, message?}`; `files {changed:
[path]}` (what the turn stored; kept for the record, the UI never lists it); `files_presented {files: [{path,
caption?, sha256, size}]}` (the whole list so far, from present_files and once more at the end when the versions
changed; the last one wins); `title {title}`;
`turn_finished {turnId, status: done|error|cancelled, error, costUsd}` (plus `code: "budget", limitUsd, ownerName`
when the budget stopped it: the UI words that in its own locale). claw's `result` is stored as it comes
(`usage`, `estimated_cost`, `message`, …); its `session` line is not stored (a server path). `tool_result.output` is
capped at 20 000 characters (`truncated: true`). The fake claw in `chat/test/fakes/claw.mjs` also emits a `debug`
event (tests only). Conversations stored before schema 6 may hold `confirmation_request`/`confirmation_resolved`
events: they still load, and the UI shows nothing for them (like any event type it doesn't know).

### `web` deployment needs

- Env: `JARVIS_ROLE=web`, `DATABASE_URL`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `JARVIS_NAMESPACE` (its own
  namespace, from the downward API; default `jarvis`), `HOSTNAME` (the pod name: the replica's name), optional
  `JARVIS_EGRESS_URL`, `JARVIS_TOOLS_URL`, `JARVIS_WORK_ROOT` (default `/var/lib/jarvis/work`, an emptyDir),
  `JARVIS_DRAIN_SECONDS` (default 1770), `JARVIS_UI_DIR`, `JARVIS_BRAND_DIR` (default `/etc/jarvis/brand`, served at
  `/brand/*`), `CLAW_PATH`. Config at `/etc/jarvis/config.json`, the system prompt file it names.
- RBAC (provider `kubernetes` only): a ServiceAccount (token mounted) bound to a Role in the sandbox namespace with
  `pods`: create, get, list, watch, delete and `pods/status`: get. Read-only for the admin monitor (optional; what
  is missing shows as a note on the page): in its own namespace `deployments` get/list (base Role `web-monitor`); the
  instance may add `metrics.k8s.io` `pods` get/list in the sandbox namespace and a ClusterRole with `nodes`,
  `metrics.k8s.io` `nodes` and `node.k8s.io` `runtimeclasses` get/list.
- Provider `e2b`: `E2B_API_KEY` (from `web-env`) and any variable `sandbox.e2b.egressHeaders` names; outbound HTTPS to
  `api.e2b.app` and `*.e2b.app`.
- Pod: `terminationGracePeriodSeconds: 1800`, readiness `GET /healthz`, a writable emptyDir at the work root; the image
  runs as uid 1000. NetworkPolicy: ingress from cloudflared only; egress to
  Postgres, the bucket, the Kubernetes API, `egress` and `tools` Services and sandbox pods on 7070.

### Public API (`web`, all behind Access; JSON)

- `GET /api/me` → `{email, name, isAdmin, product: {name, logoUrl, locale}, models: [{id, label}], defaultModel}`
- `GET /api/conversations` → newest first `[{id, title, model, updatedAt, running}]`; conversations without any
  message are left out
- `POST /api/conversations` `{model?}` → `{id}`; `PATCH /api/conversations/:id` `{title?, model?}` → 204 (the
  model applies from the next turn); `DELETE` (soft) → 204
- `GET /api/conversations/:id` → `{id, title, model, messages: [{id, role, content, turnId, attachments: [path],
  createdAt}], turns: [{id, status, model, startedAt, finishedAt, error, costUsd, presented: [{path, caption?, sha256,
  size}], events: [event]}],
  pendingConfirmations: [{id, turnId, kind, summary, detail, status}], lastEventId}` (oldest first; stream from
  `lastEventId` to continue)
- `POST /api/typing` `{conversationId?}` → 204 (keeps that conversation's sandbox, or signals the scaler; makes nothing)
- `POST /api/conversations/:id/messages` `{text, attachments: [path]}` → 202 `{turnId}`; 409 while a turn is going;
  503 while this replica drains (retry)
- `GET /api/conversations/:id/stream?after=<event id>` → SSE of events with an id greater than `after` (or the
  `Last-Event-ID` header, whichever is greater), then live ones; `id:` = event id, `data:` = one event as JSON
  `{id, turnId, seq, type, data, createdAt}`; heartbeats (`: hb`) every 15 s
- Event types: claw's (`session`, `message_start`, `text_delta{text}`, `thinking_delta`, `tool_use{id,name,input}`,
  `tool_result{id,name,output,is_error}`, `message_stop`, `result{usage, estimated_cost, …}`, `error{message}`) and
  the server's: `turn_started{turnId}`, `turn_finished{turnId, status, error?}`,
  `sandbox{status: starting|ready|reset|unavailable}`, `title{title}`, `files{changed}`, `files_presented{files}`. MCP tools
  are named `mcp__tools__<tool>`.
- `POST /api/turns/:id/cancel` → 204
- `POST /api/conversations/:id/files?name=<file name>` (raw body, `content-type` = the file's, ≤ 100 MB) → 201
  `{path, size}` (under `uploads/`, ` (2)` added before the extension when the name is taken)
- `GET /api/conversations/:id/files` → `[{path, size, source: "upload"|"agent", updatedAt}]` (the UI doesn't list it);
  `GET /api/conversations/:id/files/<path>` (each segment URL-encoded) → the bytes, `content-disposition` with the
  file name; `?v=<sha256>` = that version (immutable, cached a day); `?inline=1` = `inline` for png/jpeg/gif/webp
  (never svg); `?preview=1` = an image itself or a PDF page as a PNG ≤ 480 px wide (`&page=N`, poppler's `pdftoppm`
  in the image; 404 when there is none)
- Errors: JSON `{error: "<message>"}` with the status; 401 = no valid Access token, 403 = signed in but not on
  `config.users` (the UI shows "not on the list"); 503 while draining (the UI retries)
- Admin only: `GET /api/admin/usage?from&to` (per user: turns, model cost, tool calls, egress), `GET /api/admin/egress`,
  `GET /api/admin/monitor?tz=<IANA zone>` (the admin monitor's snapshot, below), `GET /api/admin/budget` →
  `{budget: {limitUsd, since, ownerName, spentUsd, remainingUsd, exhausted, source: config|override, override}|null,
  config, audit: [{at, actor, action, before, after}]}`, `POST /api/admin/budget` `{action: "reset"}` (spend from
  now) | `{action: "set", limitUsd}` | `{action: "clear"}` (back to config) → the same shape
- Static: the UI build (`chat/ui-dist`, `npm run build` in `chat/ui`) at `/` with the SPA fallback for `/c/<id>`
  (`index.html` never cached, hashed `app-*.js|css` immutable); `/brand/logo.svg` and `/brand/logo.png` from the
  instance (the UI falls back to the product name's initials); `/manifest.webmanifest` with `name`/`short_name`
  replaced by the product name. A new conversation's empty screen shows only the logo, a greeting and the
  composer: no description line, no example or suggested prompts (removed 2026-10-21, Deyao). The greeting
  (`chat/ui/src/greeting.ts`) is picked by Hong Kong time of day (05–12 morning, 12–18 afternoon, 18–23 evening,
  23–05 late night), at random per page load among that slot's phrases, followed by `firstName` from `GET /api/me`
  when there is one ("Good morning, Deyao" / 「早晨，Deyao」). The phrases are not made up: they come from published
  lists, CC BY-SA, cited in that file: Wikivoyage's Cantonese phrasebook and Wiktionary (午安, 晚上好, 早唞) for
  zh-HK, Wiktionary's "Category:English greetings" for en. `firstName` = the Access identity's `given_name` (else the
  first word of `name`; a CJK name whole) from the token or the team's `/cdn-cgi/access/get-identity` (Google
  sign-in); null with an e-mailed code. Never derived from the address.
- `GET /healthz` (no auth; readiness = not draining and Postgres reachable)

Authorisation: Access JWT (team domain + audience from config) → email → must be in `config.users`
(the instance's allowlist, the same list as the Access policy) → a `users` row is upserted. A user sees only their
own conversations (others' ids answer 404); admins (`config.admins`) can read the usage/egress views and the
monitor, not others' chats; to anyone else every admin path (`/api/admin/*`, `/admin`, `/admin/*`, `/admin-*` bundle
files) answers 404. A signed-in address that is not on the list gets a plain 403 page naming it (JSON 403 under `/api/`). Writes
with an `Origin` other than `publicUrl` are refused.

### Admin monitor (`/admin/monitor`, config `admins` only)

A phone-friendly page (its own bundle `admin-<hash>.js|css` + `admin.html` from `chat/ui/src/admin.tsx`, so the
staff bundle never contains it; the page may say "Jarvis 3") that polls `GET /api/admin/monitor` every 5 s while
visible. Read-only except the **Budget** panel (its own `GET/POST /api/admin/budget`: spent / limit, remaining, where
the limit comes from, reset / set limit / back to config with a confirm, the last changes). Code: `chat/src/web/monitor.ts` (one snapshot per replica cached 4 s; each section
fails on its own into `errors`). It never shows message contents: conversation titles, owners, counts, timings.

- **Sandboxes:** live rows (`starting`/`ready`/`releasing`) with conversation title, owner, conversation id (link `/c/<id>`, opens for its owner) and its number of turns (0 or "conversation deleted" is flagged: an orphan), provider + host, state,
  age, idle time, CPU/memory use vs limit (the provider's `stats()`), last bash (`events` tool_use `bash`), resets
  (earlier sandboxes of the conversation), pod restarts, vm migrations (count, last pause from `vm_events`); pool
  live / `maxActive` (vm: the hosts' slots); 24 h starts with allocation p50/p95 and recent failures with their
  reason (`ops_events`); release reasons.
- **Hosts:** `SandboxProvider.stats()` (`provider.ts`: `ProviderStats` = hosts + sandboxes + capacity + notes):
  `kubernetes` = sandbox pods + metrics-server, the nodes matching the RuntimeClass's nodeSelector or running
  sandboxes (allocatable, use, Ready, kubelet); `vm` = the provider's own `VmStats` (hub + AWS hosts with load,
  memory, slots, uptime, AWS cost so far, the scaler's settings, `vm_events` decisions and migrations), mapped by
  `normalizeStats`; `e2b` = the list only.
- **Load:** turns queued/running/waiting, claw processes and SSE streams (every web replica's presence), turns per
  hour (24 h, errors marked), queue wait (`turn_started` − insert) and turn duration p50/p95, errors by kind (turn
  errors, failed tool calls, model HTTP errors, sandbox start failures), active users now (5 min) and today (the
  browser's time zone).
- **Models & tools:** per model (the turn's model; title calls included) turns, input/output tokens (`turns.usage`)
  and USD (`turns.cost_usd` = egress model cost) today and 7 days, egress model spend; tool calls 24 h by tool with
  its service (Exa, Wisers, TikHub, `<prefix>-mcp`, web fetch), error rate, p50/p95, cost.
- **Egress:** requests per minute (1 h), denied hosts with egress's deny reason (24 h), mirror hit rate
  (`allow:cache`), per-pod cache counters, top talkers by sandbox (1 h).
- **Health:** per role its replicas from presence (kv `presence:<role>:<replica>`, every 10 s: commit from
  `JARVIS_COMMIT` = the image's build arg, start time, counters, its last 10 redacted error lines) and the
  Deployment's ready/desired/image when readable; janitor (and vm scaler) leader from `pg_locks` + `application_name`
  (`jarvis-<role>:<pod>`, set on every connection); Postgres round trip and connections by role; S3 reachability
  (a HEAD, every 30 s); the latest error lines of all replicas. Error lines are redacted where they are recorded
  (`redact` in `common/log.ts`: bearer/basic values, URL credentials, `token=`/`key=` values, long opaque strings).

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
  // provider "vm" instead (docs/SANDBOX-VM.md "Config"): {"provider": "vm", "image": …, "cpu": "1", "memory": "1Gi",
  //   "vm": {"hub": {"name", "url": "https://10.201.0.1:7443", "endpoint": "<hub IPv4>:51820"}, "cpuFeatures": "…",
  //          "spareSlots": 1, "legacyProvider"?: "kubernetes", "aws"?: {"launchTemplate", "maxHosts": 3, …}}}
  // the sandbox allowlist ("The sandbox allowlist"): the generated lists (generatedAllowlist, default true) plus
  // defaultAllowedHosts; denyHosts wins over everything; maxUploadBytes per tunnel/request (default 8 MiB)
  "egress": {"defaultAllowedHosts": [], "denyHosts": [], "generatedAllowlist": true, "maxUploadBytes": 8388608,
             "routes": [{"name": "model", "upstream": "https://openrouter.ai/api", "auth": "turn",
                         "inject": {"Authorization": "Bearer ${OPENROUTER_API_KEY}"}}], "cacheGiB": 5},
  "tools": {"exa": {"keyEnv": "EXA_API_KEY"}, "tikhub": {"keyEnv": "TIKHUB_API", "platforms": ["xiaohongshu", "douyin", "weibo", "x", "threads", "youtube", "instagram", "reddit"]},
            "wisers": {"url": "https://yyc.deyaochen.com/app/wisers/api", "accessClientIdEnv": "WISERS_ACCESS_CLIENT_ID", "accessClientSecretEnv": "WISERS_ACCESS_CLIENT_SECRET"},
            "upstreams": [{"prefix": "yyc", "url": "https://yyc-mcp.yyc-news.workers.dev/mcp",
                           "headers": {"Authorization": "Bearer ${YYC_MCP_TOKEN}", "X-YYC-User": "${USER_EMAIL}"},
                           // read-only tools to offer (default deny; "tools" above): the upstream's own names
                           "tools": ["posts_search", "post_get", "streams_list"]}]},
  "s3": {"endpoint": "...", "region": "...", "bucket": "...", "prefix": "jarvis/"},
  "budget": {"limitUsd": 30, "since": "2026-10-21T12:00:00Z", "ownerName": "Deyao"}
}
```

**Models:** `models` is what staff may pick (the UI shows no picker when there is one), `defaultModel` one of them,
`titleModel` the title call's. The API refuses any other id (`POST`/`PATCH` conversations: 400) and so does egress
(the `model` route).

**Budget** (`budget`, optional; absent = no limit; code `chat/src/common/budget.ts`): one limit for everyone, admins
included. Spend = `sum(egress_log.cost_usd)` of kind `model` (every model call: turns and titles) with `at >= since`.
Spent ≥ `limitUsd` = used up: no turn starts and egress refuses model calls (see "Turn runner", "egress"). A call
already in flight when the limit is crossed completes, so the spend can end above the limit by about one model call
per running turn (cents to a few dimes). Message (`product.locale`; the UI uses its own locale):
zh-HK「<ownerName> 設定了 <limit> 美元的使用上限，目前已用完。」, English "<ownerName> has set a USD <limit> budget
limit and it has now been used up." (no `ownerName`: 管理員 / The administrator). There is no automatic reset.
To reset or raise: edit `budget` (`since` = now resets the spend to 0; a bigger `limitUsd` raises it) and roll out the
config, or use the admin monitor's Budget panel: its override (kv `budget:override`, an audit row in `ops_events`
kind `budget` per change) wins as long as config `budget` is what it was when the override was made; editing
config `budget` afterwards takes over again.

Secrets come from the environment (`DATABASE_URL`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `OPENROUTER_API_KEY`
(egress only), tool keys (tools only); provider "vm", web only: `SBX_CA_CERT`, `SBX_CLIENT_CERT`, `SBX_CLIENT_KEY`,
`SBX_HOST_CERT`, `SBX_HOST_KEY` (PEMs) and the scaler's least-privilege AWS key `SBX_AWS_ACCESS_KEY_ID`,
`SBX_AWS_SECRET_ACCESS_KEY`). Each role gets only the Secret it needs.

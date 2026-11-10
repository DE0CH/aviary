# Jarvis 3 chat runtime

Two services of YYC AI (`ai.yyc-internal.com`), the staff AI chat whose stack is DE0CH/aviary (pigeon, magpie,
hermit): **egress**, YYC's sandbox gateway, and **tools**, the read-only tools Claude Code in a sandbox calls. The code
is in `chat/`; the instance writes their manifests itself (YYC: DE0CH/YYC `yyc-ai/deploy/egress/`, `tools/`) and pins
an image of this repo. It is a second runtime next to the task runtime in `server/` (OpenCode in Fly machines).

**What it is (Deyao, 2026-10-21): a read-only data-analysis tool, not an assistant that acts.** It searches, reads
and analyses (news, social posts, the instance's own data, uploaded files, a sandbox for code and data work); it never
sends, posts or changes anything outside. So nobody is ever asked anything: every tool is read-only (outward actions
such as sending mail are not offered at all), and the sandbox's internet access is decided automatically by an
allowlist with no manual override.

## Components (one image `ghcr.io/de0ch/jarvis3-chat`, role chosen by the first argument: `args: [egress]`)

| Role | What | Replicas |
|---|---|---|
| `egress` | The sandbox gateway (aviary's contract): hermit's sandbox tokens checked locally, the fixed names `magpie`, `hermit`, `model` (OpenRouter with forced ZDR and the budget), `tools`; the package mirror; the automatic sandbox allowlist with threat-intel DNS; its own database `egress` | 2, rolling, drained |
| `tools` | REST API (`/v1/tools`) for aviary's gateway: web search/fetch, social search, AnyAPI, upstream REST APIs' tools; read-only tools only; owner from `Aviary-Sandbox-Person`, liveness from hermit; its own database `tools` | 2, rolling, drained |

Any other first argument (or none) refuses to start (exit 2).

```
sandbox (aviary hermit's fabric; all traffic through HTTP(S)_PROXY)
   ▼
egress :3128 (hermit's sbx token checked locally, liveness from hermit)
  ├─ http://magpie/v1/sandbox/** ─▶ magpie   (pair bearer, verified identity)
  ├─ http://hermit/v1/sandbox/** ─▶ hermit   (pair bearer, verified identity)
  ├─ http://model/v1/messages ─▶ OpenRouter (key injected, ZDR forced, budget)
  ├─ http://tools/v1/tools/<name> ─▶ tools ─▶ Exa / TikHub / Wisers relay / yyc-mcp
  ├─ self hosts (selfHosts, 127.0.0.1, localhost) /mirror/… ─▶ PyPI / npm mirror
  └─ anything else ── sandbox allowlist → threat DNS → public internet
State: egress's own database `egress` (egress_log, budget_state, token_keys); tools' own database `tools` (tool_calls).
```

## Interfaces

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
     from `proxy.ts`), pins a model's providers when `models[].providers` lists them (OpenRouter endpoint slugs such
     as `deepinfra/fp8`, set as `provider.order` + `provider.only` with `allow_fallbacks: false`, over the caller's;
     endpoints differ in how they render a long tool-and-image conversation), applies the configured thinking level, refuses a model outside `models[].id`, `defaultModel`,
     `smallModel`, `titleModel` (400 `deny:model`) and every call once the budget is used up (402 `budget_exceeded`,
     `deny:budget`), streams the answer back and logs its `usage.cost` (kind `model`). No `Aviary-*` header (no staff
     identity) reaches OpenRouter. **Memory:** a request carries the whole conversation, base64 images included; the
     body is read once into one Buffer and rewritten as slices of it plus the few rewritten members (about its own
     size), at most 64 MiB of model bodies held at once (a call waits its turn), a body over 64 MiB refused with 413
     `request_too_large` before it is read. No total timeout (Claude Code's own 600 s bounds it); an upstream silent
     120 s ends the call (504 `upstream_timeout` before the answer's head, the stream cut after it).
   - `tools`: the REST paths only (decision 15, no MCP): `GET /v1/tools`, `POST /v1/tools/<name>`, `GET
     /v1/openapi.json` to egress.json's route `tools` (`http://tools`, `Authorization: Bearer $EGRESS_TOKEN_TOOLS`),
     timeout 180 s; tools learns the owner from `Aviary-Sandbox-Person`. One log row per call (kind `tools`).
2b. **Sandbox routes** (aviary platform 3.11.8, configured routes; egress.json's routes other than `model` and
   `tools`): an instance's own service the sandbox calls directly by a name (YYC: `labels`, the label store's REST API,
   `http://labels/api/v1/…`, the `labels` skill). The name is matched like a fixed name (exact, case-insensitive, no
   port or port 80); only its `allowPaths` prefixes (each `/…/`) on a clean path are routed (else 403 `not_allowed`,
   `deny:path`), any method; the headers as on a fixed name, then the route's `inject` (YYC: the Access service token
   `yyc-labels-ai`) and its `identityHeaders` (header → `sandbox`, `conversation` or `person` from the verified token,
   replacing a caller's copy; YYC: `X-Jarvis-Conversation`, which the label store keys on). An outside upstream goes
   through the exit like the model route. Timeout and drain as `tools`; one log row per call (kind `route`).
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
`egress_log_dropped_total{kind}`) and the rest written in order. Writes are idempotent: each row gets a `row_id`
(uuid, unique index) once when it is buffered, and the insert is `on conflict (row_id) do nothing`, so a batch retried
after an insert whose outcome was unknown (the client's 15 s `query_timeout`, a connection lost after COMMIT) is
written once and its model cost counts once. **Migrations** are expand-only: a database at a newer
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
  :80).
- The old relay listener (staff's own Claude Code on the model route, `EGRESS_RELAY_PORT`, table `relay_tokens`) is
  not in this egress: awaiting Deyao's decision (aviary refactor DESIGN 10, "Open: the relay").

**Configuration** `/etc/egress/egress.json` (`chat/src/egress/config.ts`): `product` (`name`, `locale` for the budget
message), `models`, `defaultModel`, `smallModel`, `titleModel`, `titleReasoning`, `contextTokens`, `routes` (exactly
`model` and `tools`, then any sandbox routes: `{name, upstream, model?, inject, allowPaths?, timeoutSeconds?,
identityHeaders?}`; a sandbox route needs `allowPaths`), `selfHosts`,
`generatedAllowlist`, `defaultAllowedHosts`, `denyHosts`, `blockCidrs`, `maxUploadBytes`, `cacheGiB`, `budget`,
`exit` (below).
**Env:** `EGRESS_DATABASE_URL`, `EGRESS_TOKEN_MAGPIE`, `EGRESS_TOKEN_HERMIT`, `EGRESS_ACCEPT_PIGEON`,
`EGRESS_ACCEPT_HERMIT`, `EGRESS_ACCEPT_ADMIN` and every variable a route's `inject` names (`OPENROUTER_API_KEY`,
`EGRESS_TOKEN_TOOLS`); a missing one is a start failure. **CI:** chat.yml's `conformance` job runs aviary's
`gateway` and `model` suites against the image under test (`chat/test/conformance/run.sh`, the binaries of
`ghcr.io/de0ch/aviary-testing` at the aviary commit in `chat/aviary.ref`, which must have a release) on the chat image built once by the `chat-image` job; `images` pushes that same image (its id
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
- **The exit (`exit` in egress.json, optional; `chat/src/egress/exit.ts`):** `{"socks": "host:port", "sandboxProfile":
  "sandbox"}`. With it, egress resolves no internet name and checks no address itself: after the port and allowlist
  checks it hands the connection to that SOCKS5 server by name (CONNECT tunnels and plain-HTTP forwards, SOCKS5
  username = `sandboxProfile`), and the server resolves through malware-blocking DNS, refuses non-public addresses and
  connects. Its refusals are logged as the same decisions: reply `0x80` → `deny:threat`, `0x04` → `deny:nxdomain`,
  `0x01` → `deny:dns`, `0x02` → `deny:address`; any other failure is a 502 (`allow`, status 502). The routes' outside
  https upstreams (the model route's OpenRouter) go through it too, on its default profile (no username); upstreams
  inside the cluster (`*.cluster.local`, `*.svc`, single-label names, private addresses) are reached directly. YYC:
  its yyc-socks pool (DE0CH/YYC `socks/`), which resolves over its nodes' IPv6 and connects IPv6 first, IPv4 after.
- **Residual risk, accepted:** an allowed site that takes uploads with credentials the attacker supplies (an API
  token planted in injected content, e.g. for GitHub) is still reachable; the sandbox holds no secrets, staff data in
  the workspace is what such an upload could carry, and the upload cap bounds it.
- **The agent is told** in its managed CLAUDE.md (the instance's prompt; YYC: hermit's configuration): only sites on
  the allowlist are reachable, a refused site cannot be enabled, and web pages are read with the `web` skill (the
  tools' web search and fetch, server-side, any public site).

### `tools` (`node src/main.ts tools`; ports 8080 REST, 9090 health)

aviary platform spec 17.2 tools: a REST API Claude Code in a sandbox calls with `curl`, as its managed skills say
(the instance's skills; YYC: DE0CH/YYC `yyc-ai/deploy/hermit/skills/`): `GET /v1/tools` (each tool's name, title, description and
inputSchema), `POST /v1/tools/<name>` (the JSON body is the arguments; the answer is the tool's text as `text/plain`,
or its JSON when it has no text, e.g. an upstream API's; bad arguments 400 `bad_request`, the tool's own error 422
`{error}`, a failure 502 `tool_failed`), `GET /v1/openapi.json` (the same as OpenAPI 3.1); the new stack's gateway
routes exactly these paths on `tools` (aviary platform 3.11.3, decision 15).

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
- **Read-only tools only** (in this role: an instance's own service that may be changed is a sandbox route instead,
  egress's `sandbox` routes). Nothing here sends, posts, writes or changes anything outside, and
  nothing asks for or waits on a confirmation. Tools (names lowercase, `snake_case`): `web_search`, `web_fetch`,
  `social_search` (TikHub), the AnyAPI tools (below), plus the **listed** tools of each configured upstream REST API,
  exposed as `<prefix>_<tool>` (YYC: prefix `data`, yyc-mcp's `/api/v1`, whose `wisers_*` tools are YYC's WiseSearch,
  service token + `X-YYC-User: <owner email>`). An upstream is described by OpenAPI 3.1 at `<url>/openapi.json`, one
  `POST <url>/<tool>` per tool (JSON in, JSON out); its 4xx `{error}` is the tool's error.
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
  calls); a failed read keeps the last list. An upstream with no list yet (its first read failed or hung) is read
  again after 5 s, 10 s, 30 s, 1, 2 and 5 minutes, then every 10 minutes: Claude Code reads the tool list once per
  conversation, so a conversation started meanwhile never sees that upstream's tools (`tools_upstream_tools{prefix}`
  shows each upstream's count; alert on 0).
- Config `/etc/tools/tools.json` (ConfigMap `tools-config`), the old shared config's `tools` section plus `listAs`:

  ```json
  {"exa": {"keyEnv": "EXA_API_KEY"}, "tikhub": {"keyEnv": "TIKHUB_API", "platforms": ["xiaohongshu", "douyin", "weibo"]},
   "anyapi": {"keyEnv": "ANYAPI_API_KEY"}, "capture": {"url": "http://capture.example/fetch", "tokenEnv": "WEB_FETCH_TOKEN", "name": "YYC"},
   "upstreams": [{"prefix": "yyc", "url": "http://mcp.example/api/v1", "listAs": "<an allowed address>",
                  "headers": {"Authorization": "Bearer ${YYC_MCP_TOKEN}", "X-YYC-User": "${USER_EMAIL}"},
                  "tools": ["posts_search", "post_get", "streams_list"]}]}
  ```
  Every variable it names must be set at start (a start failure names it, never its value).
- Env, all required: `TOOLS_DATABASE_URL`, `TOOLS_ACCEPT_GATEWAY`, `TOOLS_TOKEN_HERMIT`, plus the key variables the
  config names.
- Health on 9090: `/healthz`, `/readyz` (503 until migrations are applied, the catalogue has been read once and the
  first upstream refresh has run (bounded at 10 s per upstream: one that fails or hangs offers no tools until a later
  read, with the backoff above, works), and while draining; never for a database or hermit outage later), `/metrics`
  (`tools_requests_total{status}`, `tools_calls_total{ok}`,
  `tools_liveness_lookup_failures_total{reason="unreachable"|"unavailable"|"auth"|"bad_answer"}`,
  `tools_liveness_auth_fault`, `tools_liveness_stale_served_total`, `tools_catalogue_refused_total`,
  `tools_tool_calls_dropped_total`, `tools_drain_cut_total`, `tools_upstream_tools{prefix}`), `/version`.
- The catalogue (`/etc/tools/catalogue/catalogue.json`) is validated by hermit's rules exactly (hermit 3.4.1; the
  cases are aviary's `crates/hermit/fixtures/catalogue-cases.json`, copied byte for byte to
  `chat/test/fixtures/` and compared in chat.yml), so a file `hermit catalogue-check` passes is never refused here.
- A request body over 2 MB is answered 413 `too_large` with `Connection: close` once the body has been read.
- Liveness: only a sandbox's first lookup is waited for (at most 1 s), by the first call and those that arrive while
  it runs, so calls that arrive together are judged on the same answer; after a failed lookup, calls are admitted at
  once on the gateway's verification and the lookup is retried in the background (platform 3.11.2 step 3). hermit answering 401 or 403 is a configuration fault
  (`TOOLS_TOKEN_HERMIT`, or hermit not yet accepting it): logged once a minute, counted under `reason="auth"`,
  `tools_liveness_auth_fault 1` until hermit next answers 200 or 404 (an outage in between leaves it set). Calls are then served as in a hermit outage (platform 3.11.2: a
  hermit problem never refuses a token the gateway verified) and the pod stays ready; alert on the gauge.
- Drain (SIGTERM after the 10 s preStop): `/readyz` 503, no new connections, a request on a kept-alive connection is
  served with `Connection: close`, in-flight calls finish up to 150 s (an upstream call's own timeout is 120 s), what
  is left is cut and counted (each cut tool call is aborted and its `tool_calls` row written, `ok` false, error "cut by
  the drain"), the pending rows are written within 8 s (rows still unwritten count as dropped), the pool closes within
  2 s, exit 0 with log `drained`. `terminationGracePeriodSeconds` 180: 10 + 150 + 8 + 2 = 170 s even with the database
  blackholed. The bounds are code, not configuration. `test/tools-main.test.ts` runs this whole path in a child
  process (`run(site)` with temp paths and OS-picked ports; `main()` always runs the pod's fixed `SITE`).

## Deploying an instance

An instance writes egress's and tools' manifests itself (YYC: DE0CH/YYC `yyc-ai/deploy/egress/`, `yyc-ai/deploy/tools/`,
`args: [egress]` and `args: [tools]`, each with its own config file, database and Secrets) and pins
`ghcr.io/de0ch/jarvis3-chat:<sha12>`. A new version = bump that tag to a commit whose `chat` workflow pushed the image.

## Budget (`budget` in egress.json, optional; absent = no limit; `chat/src/egress/budget.ts`)

One limit for everyone, admins included. Spend = `sum(egress_log.cost_usd)` of kind `model` (every model call Claude
Code makes, its small/fast-model calls too) with `at >= since`. Spent ≥ `limitUsd` = used up: egress refuses every
model call with 402 `{"type":"error","error":{"type":"budget_exceeded","message":"<message>"}}` (`deny:budget`). A
call already in flight when the limit is crossed completes, so the spend can end above the limit by about one model
call per running conversation. Message (egress.json `product.locale`): zh-HK「<ownerName> 設定了 <limit> 美元的使用上限，目前已用完。」,
English "<ownerName> has set a USD <limit> budget limit and it has now been used up." (no `ownerName`: 管理員 / The
administrator). There is no automatic reset. To reset or raise: edit `budget` in egress.json (`since` = now resets
the spend to 0; a bigger `limitUsd` raises it) and roll out egress. Check:
`select sum(cost_usd) from egress_log where kind = 'model' and at >= '<since>'`.

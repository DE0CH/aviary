# hermit: sandboxes, people and secrets (spec)

Status: design for implementation, revised 2026-11-02 after review round 1 (section 29 says where every review point
landed). Authoritative inputs: `/home/claude/artifacts/magpie-target-design.md` (the target design), Deyao's decisions
in the refactor brief, the feature inventory (`refactor/inventory.json`: every feature whose `target_home` names hermit
is in the coverage table, section 27), gap reports 0-3, and the sibling specs: platform (`refactor/spec-platform.md`,
whose conventions R1-R25 this text applies throughout: `/v1/` routes, `Aviary-*` headers, camelCase bodies,
`<CALLEE>_ACCEPT_<CALLER>` credentials, namespace `yyc-ai`, crates under `crates/`), pigeon (whose section 14.2 this
spec provides), magpie, supervisor and cutover. Code references are to DE0CH/jarvis3 at 3f7499a (the live
`jarvis3-chat` image), DE0CH/claudecodeui at 3de3f5d8 and DE0CH/YYC at 1775efc4. Live numbers were read from YYC's
cluster on 2026-11-02 (read-only).

Revision 2026-11-03 (decision 13; text of record DESIGN 15): hermit and its fabric deploy onto the new Talos cluster
while the old stack and its fabric stay on the old k3s cluster. The gateway becomes a host-network pod that runs
sandboxd itself on one Talos node (14.2 rewritten), its runbook's node work goes through the Talos upgrade workflow
(14.7), hermit reaches Cluster `pg` (still on the old cluster) through Service `db/pg-shared` and the new hosts reach
the JuiceFS metadata through the gateway node (13.1, 13.5, 21). Section 30 lists every change; where other text here
still says k3s node, `yyc-pool-small-worker1`, 10.0.0.5, `10.0.0.3:6443`, `pg-rw.db`, 10.244.0.0/16 as the pod CIDR,
an API-server rule by address, fsn1 as required for hermit, or Flux, section 30 holds.

hermit has two layers, and this spec keeps them apart:

- **The contract** (section 3): the REST API every deployment's hermit answers at the fixed endpoint name `hermit`.
  pigeon, magpie, the sandbox gateway, tools and the supervisor are built against it (crate `hermit-api`) and nothing
  else. It covers who may use the deployment (people and roles), deployment facts, sandbox lifecycle (get/start for a
  conversation, warm, release, pause/resume/snapshot where an implementation has them), workspaces (`/workspace`),
  per-sandbox secrets by key, the sandbox's bootstrap descriptor and activity reports, the sandbox token keys, and the
  operator page.
- **The YYC implementation** (sections 8-22): a Rust port of jarvis3 `chat/src/web` (the `web` role) plus its host
  fabric: gVisor sandboxes on AWS burst hosts behind a WireGuard gateway, sandboxd on the hosts (mTLS), the scaler,
  packing, live migration, the janitor, JuiceFS `/workspace`, the ops snapshot and page, k8s RBAC for ops. All
  YYC-specific values live in hermit's own YYC configuration (section 9), never in pigeon, magpie or the supervisor.

Jarvis 4's implementation is out of scope (open, as instructed). Nothing here assumes it beyond the contract.

---

## 1. What hermit is, and is not

hermit decides **whether** something may run and **where**; it never decides **what** runs.

It does:
1. answer who may use the deployment and in which role (`staff`, `admin`), re-read live (section 3.4);
2. answer the deployment facts pigeon needs (product name, locale, Access teams, iOS scheme);
3. own every sandbox: one live sandbox per conversation, started on request, kept while it is used, released when idle,
   at its lifetime, on delete or when its person loses access; report each state change on a feed (section 4);
4. provide `/workspace` storage per conversation, with its quota, plus the few file operations magpie needs without a
   sandbox (list, read, clone, delete) (section 3.8);
5. mint each sandbox's identity (a signed sandbox token that the gateway verifies on its own, and a control token only
   hermit holds) and answer liveness lookups for revocation (section 6);
6. hand a supervisor everything deployment-specific it needs (the boot environment at creation and the descriptor at
   `GET /v1/sandbox/self`, section 5), and per-key secrets (section 7);
7. take demand hints (warm) for its scaler;
8. serve the operator page and its snapshot (section 18), and admin REST routes for every knob that is SQL-only today.

It does not: run or watch Claude Code (the supervisor), store conversation events, titles, shares, sent-file snapshots
or uploads as conversation data (magpie), hold the model list, budget or allowlist of internet hosts (egress), know a
person's conversations, or speak the Paseo protocol (pigeon). It never sits on the data path of a running turn: a
running sandbox's model calls, tool calls and FUSE writes keep working while every hermit replica is down (6.3).

### 1.1 Rules applied

- **Only REST between services.** Every caller reaches hermit over HTTP/1.1 JSON (plus streamed bytes for workspace
  files). Live updates are long-poll GETs. hermit's internal wake-ups between its own replicas use Postgres
  LISTEN/NOTIFY on its own database; that is never visible outside hermit.
- **Fixed endpoint names.** In the cluster, hermit is the Service `hermit` (port 80) in namespace `yyc-ai`. From inside
  a sandbox, `hermit` is the same name reached through the sandbox gateway's proxy (section 5). hermit itself calls
  `magpie` and, in YYC, `egress` (ops data) by their Service names, and the supervisor of each sandbox on :7070.
- **No deployment switches in other components.** Everything that differs between deployments and is not conversation
  state is an answer from hermit (people, roles, product, locale, sign-in teams, iOS scheme, the managed CLAUDE.md
  text, the title instructions, the sandbox's internet environment, quotas) or from `model` (models). hermit publishes
  no capability flags: an implementation that lacks a call answers it 501 `not_supported`, and every caller treats
  that as an ordinary answer, so callers run the same code against every implementation.
- **Each owns its database.** hermit has its own Postgres database `hermit` (section 10). It reads jarvis3's `jarvis`
  database only in the import job (section 22), and never again. egress and tools stop reading `sandboxes` (6.3, 6.4).
- **Names.** Crates, binaries, images, Deployments, Services, Secrets, tables, metrics and log fields say `hermit`. No
  `web`, `ui`, `jarvis_cc`, `JARVIS_*` identifiers. The host agent keeps its own name `sandboxd` (a hermit part) and
  moves into aviary with hermit. Namespaces are `yyc-ai` and `yyc-ai-fabric` (platform R14, R24).
- **Clean cut.** No `/internal/cc/*`, no `JARVIS_CC_TOKEN`, no `kv` table, no exec tokens in plaintext, no provider
  switch (`kubernetes` and `e2b` providers are dropped), no legacy-provider mode, no reading of `config.json`. Existing
  sandboxes are not adopted: the new fabric is built beside the old and the old is removed after the switch
  (section 22).
- **Zero downtime** (section 20): at least 2 replicas, surge-first rolling updates, readiness, drains that hand work
  over instead of waiting it out, Postgres advisory-lock leadership for the singleton loops (janitor, scaler), and
  every long task (start, migration, release) held by a renewable row claim that any replica can take over. The one
  stated gap is the single gateway node (14.7).
- **Read-only product.** hermit adds no confirmations and no outward actions for staff. Its only outward actions are
  infrastructure (EC2, sandboxd) done for its own job.

### 1.2 What moves out of today's `web`

| Today in `web` (jarvis3) | Goes to | Why |
|---|---|---|
| `ops_events` kind `view_as` (39 rows) and `POST /internal/cc/view-as` | magpie (`POST /v1/view-as`, pigeon M17), imported once | the audit names conversations and sends; magpie owns conversation events |
| "conversations per staff member" on the ops page (counted from `sandboxes` rows, `ops.ts:490-519`) and `people.deleted` (= `release_reason='deleted'`) | magpie `GET /v1/ops/people` (section 18.4) | magpie knows conversations and deletions; counting sandboxes miscounted (A17.F5) |
| the view-as log section of the ops page | magpie `GET /v1/ops/view-as`, shown by hermit | same |
| `userKey` (`'u' + sha256(email)[:24]`) for `users/<key>/claude` and `cc_<key>` | magpie (its import uses it); the same derivation is the log field `person_key` (platform 3.8) | `~/.claude` is the supervisor's FUSE view of magpie, not a JuiceFS folder |
| `~/.claude` folder per person on JuiceFS (`users/<key>/claude`), bound into every sandbox | magpie (storage) + supervisor (FUSE) | target design |
| sent-file snapshot route (`JARVIS_SENT_FILES_URL`, egress route `sent-files`, `send-file-hook.mjs`, `JARVIS_CLAUDE_SETTINGS`) | magpie + supervisor | target design ("sent-file snapshots") |
| title generation (fork `jarvis-title.ts`, its `TITLE_SYSTEM` text) | the supervisor runs magpie's `title` command against `model`; the instructions text is hermit's descriptor content (`title.system`, 3.7.1) | titles are conversation state; the instructions are deployment content |
| the managed MCP file (gone, decision 15) → the managed skills (`yyc/deploy/skills`) | hermit's config → descriptor `managedSkills` → supervisor writes them | deployment content, like the CLAUDE.md |
| Claude Code env (`ANTHROPIC_*`, `CLAUDE_CODE_*`, telemetry off) | supervisor (fixed), model ids from `model` | no model names in hermit |
| uploads staged into the workspace before a turn | supervisor (materialises magpie's blobs into `/workspace/uploads`, supervisor 12.1) | one owner; hermit has no upload route |
| the old chat UI tables in `jarvis` (users, conversations, messages, turns, events, claw_sessions, files, shares, blobs, blob_grants, confirmations, allowed_hosts) | archived, then dropped (Deyao, decision 5); egress backfills its person column first (A13.F5, section 22.4) | not hermit's |
| `egress_log`, `tool_calls` | stay egress's / tools' | not hermit's |
| `kv` (`sandbox_demand:*`, `egress_pod:*`, stale `turn_sandbox:*`, `replica:*`, `presence:*`) | `demand_hints` table; egress targets from EndpointSlices (section 13.8); the rest is dropped with the `jarvis` database's hermit tables | typed tables, no kv |

---

## 2. Position in the system

```
          pigeon ──(people, deployment, warm, /ops)───────────────────────┐
                                                                          v
  magpie ──(sandbox for a conversation, release, events feed, workspaces)──>  hermit (>=2 replicas)
                                                                          ^   │  │  │  Postgres db/hermit (own DB)
                                                                          │   │  │  └─ k8s API (pods, deployments, endpointslices)
  supervisor in the sandbox ──proxy──> gateway ──(/v1/sandbox/self,        │   │  └──── EC2 API (AWS burst hosts)
    │  ^          (YYC: egress :3128     secrets, activity)────────────────┘   │
    │  │           over the fabric VIP)  ──(/v1/sandbox/** to magpie)──> magpie│ mTLS :7443 over WireGuard
    │  │                                 (token keys, liveness, cached)        │
    │  └──── :7070 control (hermit only: health, release, prepare) <───────────┤
    │  gVisor sandbox on an AWS host (sandboxd) <──────────────────────────────┘
    │  /workspace = JuiceFS ws/<conversation>
  yyc/monitor ──(GET /ops/snapshot, monitor token)──> hermit;   operator (Claude) ──(admin token)──> hermit /v1/admin/*
```

A turn (from the target design, hermit's part in bold): app -> pigeon -> magpie `POST /v1/conversations/{id}/turns`
-> **magpie `PUT http://hermit/v1/conversations/{id}/sandbox`** -> **200 ready, or 202 and magpie long-polls `GET`
until ready** (magpie emits `sandbox preparing/ready` for the chip) -> magpie queues `start_turn` for the conversation
-> the supervisor, which took magpie's lease when it booted, receives it on its command long-poll (magpie never calls
the sandbox) -> the supervisor runs Claude Code, writes through FUSE into magpie and **reports activity to hermit every
15 s** -> the turn ends -> after 30 min without activity **hermit releases the sandbox** (the supervisor agrees it is
idle first). If the sandbox dies, **hermit's event feed says `gone` with `sandbox_lost` within 10 s** and magpie ends
the turn.

---

## 3. The contract (REST API, crate `hermit-api`)

### 3.1 Conventions

- Base URL `http://hermit`. Every route is under `/v1/` except the operator page `/ops/` and `/ops/snapshot` (browser
  paths, platform 3.4). From inside a sandbox only `/v1/sandbox/**` exists, reached as absolute-form requests through
  the gateway's proxy (5.1). HTTP/1.1, JSON bodies (`content-type: application/json; charset=utf-8`), camelCase field
  and query names (platform 3.6), at most 1 MiB per JSON request (413 `too_large`).
- Every response: `cache-control: no-store`, `x-content-type-options: nosniff`.
- Errors: `{"code": "<lower_snake_case>", "message": "<English, for logs>"}` plus type-specific fields where stated.
  The codes `not_allowed`, `sandbox_busy`, `sandbox_failed`, `sandbox_lost`, `sandbox_unreachable` are the ones
  magpie maps to its turn codes and pigeon renders; hermit never sends staff-facing sentences.
- Times are RFC 3339 UTC with milliseconds (`2026-11-02T04:05:06.789Z`, the same format today's `toISOString()`
  gives). Ids are lowercase UUIDs. Emails are trimmed and lowercased by hermit before any comparison.
- Unknown request fields are ignored; callers ignore unknown response fields (the contract grows additively; nothing
  is ever renamed in place).
- Headers (platform 3.5): hermit logs `Aviary-Request` as `request` and forwards it on every call it makes for that
  request (sandboxd, magpie, egress, the supervisor); for its own background work it mints a v7 id. Clients send
  `Aviary-Protocol: hermit-core=1` (pigeon, magpie, gateway, tools, admin, monitor) or `hermit-sandbox=1` (the
  supervisor); hermit accepts the current and previous number of each surface.
- An implementation that lacks a call answers 501 `not_supported`; that is a normal answer, never a reason for a
  caller to read configuration.

### 3.2 Callers and credentials

One pair token per caller -> hermit (platform 3.7): 32 random bytes, base64url (43 characters). hermit reads
`HERMIT_ACCEPT_<CALLER>` (comma-separated, current first, two during a rotation), compares SHA-256 digests in constant
time (`ring::constant_time::verify_slices_are_equal`) and maps the token to a caller. A token is valid only for its
caller's routes; anything else is 403 `wrong_caller`. Missing or unknown token: 401 `unauthenticated` with
`www-authenticate: Bearer`. The route-to-caller table is code (`aviary-core::auth`), not configuration.

| Caller | Env (hermit side) | Routes | Identity header |
|---|---|---|---|
| pigeon | `HERMIT_ACCEPT_PIGEON` | `GET /v1/deployment`, `GET /v1/people/{email}`, `POST /v1/warm`, `GET\|HEAD /ops/*` | `Aviary-Person` (required on `/ops/*`: must be an admin now) |
| magpie | `HERMIT_ACCEPT_MAGPIE` | `GET /v1/deployment`, `GET /v1/people`, `GET /v1/people/{email}`, `/v1/conversations/{id}/sandbox*`, `GET /v1/sandbox-events`, `/v1/workspaces/**` | none (the person is in the body where it matters) |
| gateway (YYC: egress) | `HERMIT_ACCEPT_GATEWAY` | `GET /v1/sandbox-token-keys`, `GET /v1/sandbox-tokens/{sandboxId}`, the forwarded `/v1/sandbox/**` | `Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation`, `Aviary-Sandbox-Person` on `/v1/sandbox/**` (6.3) |
| tools | `HERMIT_ACCEPT_TOOLS` | `GET /v1/sandbox-token-keys`, `GET /v1/sandbox-tokens/{sandboxId}` | none |
| monitor (YYC news stream) | `HERMIT_ACCEPT_MONITOR` | `GET\|HEAD /ops/snapshot` | none (read-only ops scope) |
| admin (the operator: Claude, runbooks) | `HERMIT_ACCEPT_ADMIN` | `/v1/admin/**`, `GET\|HEAD /ops/*` | `Aviary-Admin: <email>` (required, recorded in `admin_events`) |

hermit's own outbound pair tokens: `HERMIT_TOKEN_MAGPIE` (magpie's ops reads, workspace references and the person
purge) and `HERMIT_TOKEN_EGRESS` (egress's ops summary). To the supervisor it presents the sandbox's control token
(6.2). pigeon, magpie and the gateway strip any client-supplied `Aviary-*` header before forwarding; hermit trusts
those headers only together with that caller's token.

### 3.3 Deployment facts

#### H1 `GET /v1/deployment` (pigeon, magpie)

200:
```json
{
  "product": { "name": "YYC AI", "locale": "zh-HK" },
  "signIn": {
    "access": [ { "teamDomain": "yyc-hk.cloudflareaccess.com", "audience": "21cd5332…349138" } ],
    "appScheme": "dev.de0ch.yycai"
  }
}
```
- `product`, `signIn` exactly as pigeon 4.1 reads them. `appScheme` may be null (iOS sign-in off).
- No capability flags (1.1). Pause, resume, snapshot and secrets either work or answer as 3.6.4 and 3.7.2 say.
- Served from memory (config loaded at start); never touches Postgres. Size < 4 KiB.

### 3.4 People: who may use the deployment

#### H2 `GET /v1/people/{email}` (pigeon, magpie)

`{email}` percent-encoded. 200 always for a well-formed address:
```json
{ "email": "staff@yyc.hk", "allowed": true, "role": "staff" }
```
- `role`: `"admin"` | `"staff"` | `null`; `allowed:false` <=> `role:null`. An admin is always allowed.
- 400 `bad_email` if it is not `local@domain` with exactly one `@`, at most 254 bytes, no whitespace or control
  characters.
- Answered from the in-memory catalogue (3.4.1); never touches Postgres; p99 < 1 ms in-process.

#### `GET /v1/people` (magpie, and hermit's own ops page)

200:
```json
{
  "generation": "c0ffee…(sha256 hex of the catalogue file)",
  "loadedAt": "2026-11-02T04:00:00.000Z",
  "explicit": [ { "email": "chendeyao000@gmail.com", "role": "admin" }, { "email": "tony.leung@example.com", "role": "staff" } ],
  "domains": [ "yyc.hk" ]
}
```
`domains` are the `@domain` entries without the `@` (everyone at that domain is `staff`).

#### 3.4.1 The YYC catalogue

- File `YYC/yyc-ai/deploy/catalogue.json`: `{"users": [...], "admins": [...]}` with today's values moved out of
  `config.json` unchanged (`users`: `@yyc.hk` plus 6 explicit addresses; `admins`: `chendeyao000@gmail.com`). It is
  YYC's staff catalogue, read by hermit and tools (ConfigMap `yyc-ai/catalogue`), Cloudflare Access (tofu
  `access.tf`), and yyc-mcp with its `mcp-cleanup` CronJob (ConfigMap `yyc/yyc-catalogue`, whose key stays
  `config.json`; 21.5 has every reader and the commit that moves them).
- hermit mounts ConfigMap `catalogue` (generated with `disableNameSuffixHash: true`, key `catalogue.json`, mounted as
  a directory at `/etc/hermit/catalogue`, no `subPath`, so the kubelet updates the file in place within about 60 s and
  no pod rolls).
- hermit re-reads it every 30 s (and at start). A file whose SHA-256 is unchanged is skipped. Validation: valid JSON
  object; `users` a non-empty array of strings, each an email (H2 rules) or `@domain` (`@` + a hostname of letters,
  digits, `-`, `.`); `admins` an array of emails, each matched by `users` (exactly or by domain); at most 10 000
  entries, file at most 1 MiB. A file that fails validation is refused: hermit keeps the last good catalogue, logs
  `catalogue refused` with the reason every 5 min, counts `hermit_catalogue_refused_total`, and the ops verdict shows a
  `problem` finding "Staff list could not be loaded: <reason>". At start with no good copy, hermit is not ready (20.1).
  The same rules run in YYC's CI as `hermit catalogue-check` (platform 13.3 item 7).
- `allowed(email)`: exact match in `users`, or `users` has `@<domain>` where `<domain>` is the part after the last `@`
  (exact, no subdomain matching, as today). `role`: `admin` if in `admins`, else `staff` if allowed, else null.
- Taking the deployment offline is the tofu Access policy swap (gap-1 3.1), not an empty catalogue (`users` must be
  non-empty, so a broken edit can never lock everyone out). pigeon closes sockets of people hermit says are not allowed
  within 75 s (pigeon 4.4), so removing someone from `users` takes effect everywhere within about 2 min (kubelet sync +
  30 s re-read + caches). Removal blocks use; it deletes nothing (3.12, 16: the person purge is a separate operator
  act).

### 3.5 Warm (demand hint)

#### H3 `POST /v1/warm` (pigeon)

Body `{"person": "<email>", "conversationId": "<uuid>" | null}`.
- 202 `{"hinted": true}` when the person is allowed and the conversation (if given) has no `starting`/`ready`
  sandbox and this replica is not draining: upsert `demand_hints (person, at=now())`. 202 `{"hinted": false}` otherwise
  (live sandbox, draining). 403 `not_allowed` for a person who is not allowed (pigeon logs it at debug).
- Never creates, touches or extends a sandbox, never resets idle time (A3.F3). The scaler counts hints younger than
  120 s (13.4). pigeon coalesces to once per (person, conversation) per 30 s per replica; N pigeon replicas may send N
  hints, which is harmless.

### 3.6 Sandboxes (magpie)

#### 3.6.1 The `Sandbox` object

```text
Sandbox {
  id: uuid,                    // a new id for every new sandbox of a conversation (the "generation")
  conversationId: uuid,
  person: email,
  state: "starting" | "ready" | "paused" | "releasing" | "gone",
  phase: null | "capacity" | "workspace" | "placing" | "host" | "image" | "creating" | "supervisor",   // only while starting
  version: int,                // +1 on every change of this row that a caller can see (state, phase, busy)
  createdAt: iso, readyAt: iso|null, lastActiveAt: iso, expiresAt: iso, releasedAt: iso|null,
  busy: bool,                  // from the supervisor's last activity report (3.7.3): state != "idle"
  image: "sha256:<64 hex>"|null,
  supervisor: null | { protocol: int, version: string, bootId: string },   // information only, from its reports
  release: null | { reason: ReleaseReason, note: string|null, code: "sandbox_released"|"sandbox_lost"|"sandbox_busy"|"sandbox_failed", message: string }
}
```
- The object carries no address and no credential of the supervisor: only hermit talks to :7070 (3.10), and the
  control token never leaves hermit (6.2).
- `lastActiveAt` is the idle clock: the later of the last `PUT` for the conversation and the supervisor's activity
  (3.7.3); magpie's watchdog reads it (magpie 8.8).
- `ReleaseReason` is one enum, shared with the supervisor's release call (3.10) and the ops page (13.13 lists every
  value, its code and its source). `note` is free text (at most 200 characters: the operator's note, or the class of a
  start error); `message` is English detail for logs, at most 300 characters, redacted (no tokens, no URLs with
  credentials).
- `release.code`: `sandbox_released` (an orderly end: idle, lifetime, pool, deleted, offboarded, operator),
  `sandbox_lost` (it died: host lost, out of memory, vanished, unhealthy, migration lost it), `sandbox_busy` (no
  capacity: never became ready), `sandbox_failed` (start failed for another reason).

#### 3.6.2 `PUT /v1/conversations/{cid}/sandbox` — get or start

Body `{"person": "<email>"}`. Idempotent per conversation: any number of calls, from any magpie replica, yield the
same live sandbox. Every `PUT` that finds or makes a live sandbox sets `last_active_at = now()` and
`reserved_until = now() + reserveSeconds` (120 s), so the janitor's idle rule and the pool-victim choice leave that
sandbox alone until the turn has reached the supervisor and its activity reports take over (A20 race: a sandbox idle
for 29:59 picked between `PUT` and `start_turn`).

| Case | Answer |
|---|---|
| A `ready` sandbox exists for `cid` | 200 `Sandbox` (state ready). If its last activity report is older than 45 s, hermit also schedules the silence check of 12 step 3 for it in the background; the answer never waits for it and `PUT` itself never releases anything |
| A `starting` sandbox exists | 202 `Sandbox` (starting) |
| A `releasing` sandbox exists | hermit waits up to 5 s for it to become `gone`, then starts a new one: 202; if it is still releasing after 5 s: 202 with the new row queued behind it (phase `capacity`) |
| A `ready` sandbox with a pending soft release (13.12) | the pending release is cancelled; 200 `Sandbox` |
| No live sandbox | insert a `starting` row (section 11), 202 `Sandbox` |
| person not allowed | 403 `not_allowed` (nothing created) |
| a live sandbox of `cid` belongs to another person | 409 `person_mismatch` (a magpie bug; logged as an error) |
| sandboxes disabled by the operator (16) | 503 `sandbox_busy` `{reason: "disabled", retryAfterS: 300}`, header `retry-after: 300` |
| `paused` (implementations with pause) | hermit resumes it: 202 (starting) |
| bad uuid | 400 `bad_request` |

Response time: p99 < 50 ms. The start itself never runs inside this request. hermit never answers 402 (a model budget
refusal is egress's and reaches magpie as `model.budget`, magpie 8.9) and never 429.

#### 3.6.3 `GET /v1/conversations/{cid}/sandbox?waitMs=&afterVersion=`

- Without `waitMs` (or 0): 200 the live `Sandbox` of `cid`, or the most recent `gone` one if there is no live one and it
  ended less than 10 min ago (so a caller sees why a start failed), else 404 `no_sandbox`.
- `waitMs` 1..25000: if the sandbox's `version > afterVersion` (default: the version current at the call) or its state
  is not `starting`, answer at once; otherwise hold the request until its version changes or `waitMs` passes, then
  answer 200 with the current object (unchanged version = timeout). Wake-ups come from LISTEN/NOTIFY inside hermit
  (10.4); a replica that misses a notify re-reads every 1 s while holding requests. A held request holds no database
  connection.
- magpie's pattern (magpie 8.3): `PUT`, then if 202, loop `GET ?waitMs=25000&afterVersion=<v>` until `ready` or `gone`,
  with a 35 s client timeout per call. There is no fixed-interval poll. Any hermit replica answers; a replica restart
  costs one retry.

#### 3.6.4 Release, pause, resume, snapshot

- `POST /v1/conversations/{cid}/sandbox/release` body `{"reason": "deleted" | "offboarded" | "requested"}`: releases
  the live sandbox of `cid` (13.12). 200 `{"released": 0|1, "sandbox": Sandbox|null}` at once (the release runs in the
  background; the feed reports `releasing` then `gone`). Idempotent. `deleted` and `offboarded` are forced (a running
  turn is ended cleanly by the supervisor, then the sandbox is destroyed); `requested` is soft (done at the next moment
  the supervisor reports idle). magpie calls it from its outbox on soft delete, permanent delete and purge (A1.F20,
  A17.F0, A17.F4, A17.F5). A soft delete releases too: a deleted conversation has nothing to keep running, and a
  restore gets a new sandbox on its next turn.
- `POST /v1/conversations/{cid}/sandbox/pause`, `/resume`, `/snapshot`: the contract shapes are pause -> 202 `Sandbox`
  (state `paused`; processes frozen, no CPU, keeps memory or a checkpoint), resume -> 202 `Sandbox` (`starting` then
  `ready`, same id, same `/workspace`), snapshot -> 201 `{"snapshotId": uuid, "at": iso}` (a restorable image of the
  sandbox's scratch and processes). In the YYC implementation all three answer 501 `not_supported`: YYC sandboxes are
  disposable and nothing outside `/workspace` and magpie is promised to survive a release. Nothing in the product
  uses them: share freeze and continue use `POST /v1/workspaces/{wid}/clone` only (3.8; magpie 12, magpie R28).
  (A question waiting for its answer is no reason to pause either: DECISIONS 17 rules a checkpoint, freeze or pause
  out for it, in any case. Such a sandbox is idle (3.7.3) and released; its turn is parked in magpie and resumed in a
  new sandbox when the person answers, supervisor 7.6.)

#### 3.6.5 `GET /v1/sandbox-events?after=<seq>|latest&limit=&waitMs=` — the change feed

```json
{
  "headSeq": 98123,
  "events": [
    { "seq": 98122, "at": "…", "sandboxId": "…", "conversationId": "…", "person": "…",
      "state": "gone", "phase": null, "version": 7, "busy": false,
      "release": { "reason": "host_lost", "note": null, "code": "sandbox_lost", "message": "aws-3f1c2a stopped answering for 30 s" } }
  ]
}
```
- One event per visible state or phase change (not per heartbeat; `busy` flips are included). `seq` is a bigserial
  inserted in the same transaction as the change; event inserts are serialised so they commit in seq order and a
  reader never skips one (10.5). Seq may have holes after a rolled-back transaction; callers rely only on order.
- `after=latest` answers `{headSeq, events: []}` at once. `limit` 1..500 (default 200). `waitMs` 0..25000: hold until
  an event with `seq > after` exists.
- Retention 7 days; `after` older than the oldest kept event minus 1 -> 410 `cursor_expired` (magpie then re-reads the
  live sandboxes of its running turns with 3.6.3 and continues from `latest`).
- This pull feed is the only path from hermit to magpie for sandbox state (platform R8): hermit pushes nothing.
  magpie's job leader follows it, ends turns whose sandbox is `gone`, and re-acquires a sandbox for a turn that had not
  reached the supervisor when its sandbox was released (magpie 8.7, 28).

### 3.7 The sandbox's own routes (`/v1/sandbox/**`, the supervisor through the gateway)

Reached from inside a sandbox as `http://hermit/v1/sandbox/…` through the gateway's proxy (5.1). The gateway verifies
the sandbox (6.3) and forwards with its own pair token and the three `Aviary-Sandbox-*` headers. hermit answers only
for a sandbox in `starting`, `ready`, `paused` or `releasing` whose id, conversation and person match its row (else 404
`no_sandbox`). The types (`SandboxSelf`, `ActivityReport`, `ActivityReply`) are defined once in `hermit-api` and
re-exported by `supervisor-api`, so the two sides compile against one definition.

#### 3.7.1 `GET /v1/sandbox/self` — the descriptor (`SandboxSelf`)

The supervisor's first call (section 5). `If-None-Match` honoured (304). 200, at most 64 KiB:
```json
{
  "sandbox": { "id": "…", "conversationId": "…", "person": "staff@yyc.hk",
               "createdAt": "…", "expiresAt": "…", "image": "sha256:…" },
  "control": { "port": 7070, "tokenSha256": ["<64 hex>"] },
  "claude": { "managedClaudeMd": "<the deployment's managed CLAUDE.md text>" },
  "title": { "system": "<the deployment's title instructions>" },
  "env": { "PIP_INDEX_URL": "http://sbx:hst1.…@10.202.1.1:3128/mirror/pypi/simple", "PIP_TRUSTED_HOST": "10.202.1.1",
           "npm_config_registry": "http://sbx:hst1.…@10.202.1.1:3128/mirror/npm/" },
  "envPrivate": [],
  "resources": { "memoryBytes": 1073741824, "cpus": 1, "pids": 4096, "tmpBytes": 5368709120, "openFiles": 65536,
                 "workspace": { "path": "/workspace", "quotaBytes": 1073741824, "quotaInodes": 1000 } },
  "lifecycle": { "activityEverySeconds": 15, "idleMinutes": 30, "maxLifetimeHours": 8, "lifetimeGraceMinutes": 60 },
  "secrets": { "keys": [] },
  "etag": "\"b41c…\""
}
```
- `control.tokenSha256`: the SHA-256 (hex) of each control token hermit may present to :7070 (6.2). A list so a
  rotation can overlap; YYC sends one entry. The supervisor compares `sha256(presented bearer)` with each on every
  inbound call; the plaintext never enters the sandbox.
- `claude.managedClaudeMd`: the text the supervisor writes to `/etc/claude-code/CLAUDE.md`. YYC: the instance prompt
  (`system-prompt.md`) + a blank line + the sandbox rules (`sandbox-rules.md`) with `{workspaceGiB}`,
  `{workspaceFiles}`, `{tmpGiB}` replaced by the values in `resources` (gap-2 2.4: the rules state what hermit
  actually provisions).
- `title.system`: the instructions the supervisor sends with magpie's `title` command (supervisor 14). YYC: file
  `yyc/deploy/hermit/title-system.md`, today's `TITLE_SYSTEM` moved verbatim from claudecodeui
  `server/modules/jarvis/jarvis-title.ts:23`.
- The boot environment (5.1) already holds the proxy variables, set when the sandbox was created; the supervisor reads
  the proxy from its environment and hard-codes nothing (platform R13). `env` holds the further variables every process
  the agent starts must have (YYC: the package mirrors through the gateway); the supervisor applies them to Claude
  Code, dockerd and exec children and never interprets them. `envPrivate` names boot-environment variables the agent
  must not inherit (YYC: none; the agent needs the proxy). At most 128 entries and 16 KiB together.
- `etag`: `"` + the first 32 hex of SHA-256 over the canonical JSON of every other field + `"`. The answer changes
  only when hermit's configuration changes (a hermit roll with a new prompt, title text or env); the supervisor
  re-reads it at start, at every `start_turn` and every 10 min, and applies a change at its next idle point
  (supervisor 6.4). During a hermit roll two replicas may briefly answer different etags; the supervisor's apply is
  idempotent, so that costs at most one extra rewrite.

#### 3.7.2 `GET /v1/sandbox/secrets` and `GET /v1/sandbox/secrets/{key}`

- `GET /v1/sandbox/secrets` -> 200 `{"keys": ["<key>", …]}` (YYC: `[]`).
- `GET /v1/sandbox/secrets/{key}` (`key` = `[A-Z][A-Z0-9_]{0,63}`) -> 200 `{"key": "…", "value": "…"}` for a granted
  key, 404 `not_granted` otherwise. An empty grant is a valid answer (target design). YYC grants nothing, so every key
  is 404; no secret value ever enters a YYC sandbox (A3.F14, A5.F18). Section 7 has the model.

#### 3.7.3 `POST /v1/sandbox/activity` — liveness and busy (`ActivityReport`)

Body:
```json
{ "state": "running", "turnId": "0190f6c2-…", "tasks": 2, "idleSince": null, "lastOutputAt": "…",
  "bootId": "…", "protocol": 1, "version": "0.1.0+3f2a1c", "restarts": 0, "rssBytes": 31457280,
  "memory": { "limit": 1073741824, "used": 612368384 },
  "events": [ { "kind": "memory_kill", "pid": 912, "comm": "python3", "rss": 402653184, "at": "…" } ] }
```
- Sent by the supervisor every `lifecycle.activityEverySeconds` (15 s) and at once on every change of `state`.
  `state`: `starting`, `running` (a turn), `stopping` (an interrupt in progress), `held` (background tasks after the
  turn), `asking` (the run waits only on a person's answer to an open question: no tool call or background task
  runs; supervisor 7.6, DECISIONS 17), `idle`, `released`. `busy` is false for `asking`, `idle` and `released`, true
  otherwise (A9.F3, A9.F16, A11.F16): the time a question waits is idle time, so the idle release (30 min) and the
  other soft releases reach such a sandbox, whose supervisor then parks the turn rather than ending it. `idleSince` is
  set in `idle` and in `asking` (when the question opened).
- hermit sets `last_heartbeat_at = now()`, `busy`, `busy_turn`, `background_tasks = tasks`, `boot_id`,
  `supervisor_version/protocol`; while busy also `last_busy_at = now()` and `last_active_at = now()`; on a busy -> idle
  transition `last_active_at = greatest(last_active_at, least(now(), idleSince))` (the idle clock starts when the work
  ended, never earlier than the last `PUT`). One row update per
  call; a `busy` flip also writes an event (3.6.5). Each `events[]` entry of kind `memory_kill` becomes a
  `host_events` row (`memory-kill`, shown on the ops page).
- 200 `ActivityReply`: `{"state": "ready" | "releasing", "release": null | {"reason": ReleaseReason, "code": "…",
  "force": bool, "deadlineAt": iso}}`. A non-null `release` repeats a pending release (13.12) in case hermit's direct
  call to :7070 was lost. The supervisor must act on it exactly as on `POST /v1/lifecycle/release` with the same body
  (3.10): with `force: false` it releases only if it is idle, otherwise it keeps working and its next report (busy)
  tells hermit the release was refused.
- Replaces `POST /internal/cc/touch` every 60 s from the UI (A1.F10, A3.F1): activity is reported by the sandbox's
  owner, not inferred from a socket elsewhere.

### 3.8 Workspaces (`/workspace`)

`/workspace` is storage hermit provides. A workspace has an id (uuid). The convention, which magpie owns: a
conversation's workspace id = its conversation id; a share's frozen workspace id = `uuidv5(SHARE_NS, "share:" +
shareId)` with `SHARE_NS = 6f1c9d2e-0b7a-4f53-9a43-5b2f3f1e8c61`, for new and imported shares alike (magpie 12,
section 22.3). The sandbox of conversation `c` always mounts workspace `c`.

```text
Workspace { id: uuid, createdAt: iso, quota: { bytes: int, inodes: int }, usage: { bytes: int, inodes: int } }
Entry { name: string, type: "file"|"dir"|"symlink"|"other", size: int, mtime: iso }
```

| Call | Request | Success | Errors |
|---|---|---|---|
| `PUT /v1/workspaces/{wid}` | `{}` | 201 `Workspace` (made, quota set); 200 (existed; quota set again if missing) | 503 `workspace_unavailable` |
| `GET /v1/workspaces/{wid}` | | 200 `Workspace` | 404 `no_workspace` |
| `GET /v1/workspaces/{wid}/entries?path=<rel>&cursor=` | `path` default `""` (the root) | 200 `{path, entries: [Entry], nextCursor}` (sorted by name, 1 000 per page) | 404 `no_path`; 400 `bad_path`; 409 `not_a_dir` |
| `GET /v1/workspaces/{wid}/files?path=<rel>` | `Range` (one range) | 200/206 bytes; `content-length`, `last-modified`, `etag: "<size>-<mtime ns>"`, `accept-ranges: bytes`, `content-type: application/octet-stream` | 404 `no_path`; 409 `not_a_file`; 416 |
| `POST /v1/workspaces/{wid}/clone` | `{"from": "<wid>"}` | 201 `Workspace` (copy-on-write copy of `from`, own quota) | 404 `no_workspace` (from); 409 `exists` |
| `DELETE /v1/workspaces/{wid}` | | 204 (also when absent) | 503 `workspace_unavailable` |

How magpie uses them (one model, no other primitive):
- **Live read** (files panel downloads, paths named in replies): `GET /v1/workspaces/{conversationId}/files?path=`.
- **Share freeze**: `POST /v1/workspaces/{shareWs}/clone {"from": "<conversationId>"}` where `shareWs =
  uuidv5(SHARE_NS, "share:" + shareId)`.
- **Continue from a share**: `POST /v1/workspaces/{newConversationId}/clone {"from": "<shareWs>"}`.
- **Revoke a share**: `DELETE /v1/workspaces/{shareWs}`. **Permanent delete / purge**: `DELETE
  /v1/workspaces/{conversationId}` plus each live share's workspace.

Rules:
- Paths: relative, UTF-8, no NUL, no `.` or `..` components, at most 4 096 bytes and 255 bytes per component;
  resolved with `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` from the workspace root, so a symlink the agent made
  can never lead outside the workspace. Reads follow symlinks inside the workspace; `entries` reports them as
  `symlink`.
- There is no write route: uploads are written into `/workspace/uploads` by the supervisor from magpie's blobs
  (supervisor 12.1), inside a sandbox whose workspace hermit ensured, with its quota, before the sandbox was created
  (start step 2). That keeps the A18.F3 fix (no workspace without a quota) without a second writer.
- `clone` of a workspace whose conversation has a `ready` sandbox: hermit first asks that supervisor to flush
  (`POST :7070/v1/lifecycle/prepare {"kind": "snapshot", "force": false, "deadlineMs": 10000}`, 3.10: a `sync(2)` so
  every write the sandbox made is on JuiceFS), then clones. A failed or slow flush is logged and the clone goes ahead
  (it then holds what JuiceFS had committed, which for closed files is everything). `clone` is metadata-only in YYC
  (`juicefs clone`, section 15), so a share or a continue no longer copies bytes (A20.F22: 6.4 s today).
- `DELETE` of a conversation's workspace first releases its live sandbox (reason `deleted`, forced), waits until it is
  gone (at most 60 s), then removes the tree and its quota.

### 3.9 Sandbox token keys and liveness (gateway, tools)

The sandbox token is self-verifying (6.1): the gateway checks it with hermit's public keys and asks hermit only
whether the sandbox is still live, with caching that survives a hermit outage (6.3).

#### `GET /v1/sandbox-token-keys`

200 `{"keys": [{"kid": 1, "alg": "Ed25519", "publicKey": "<base64url 32 bytes>"}]}`: one entry per key in
`HERMIT_TOKEN_KEYS`. Changes only with a hermit roll that adds or drops a key. Callers refetch every 5 min and at once
(at most every 10 s) when a token names an unknown `kid`; on a failed fetch they keep the last set indefinitely.

#### `GET /v1/sandbox-tokens/{sandboxId}`

200:
```json
{ "sandboxId": "…", "conversationId": "…", "person": "staff@yyc.hk",
  "state": "ready", "live": true, "expiresAt": "…" }
```
- `live` = the row is not `gone` and its person is allowed by the current catalogue (so a removed person's sandbox
  stops being usable within one cache period, before the janitor's release; fixes A17.F17). 404 `unknown_sandbox` for
  an id hermit has no row for.
- Served from an in-memory map of every non-gone sandbox per replica, kept current by the `hermit_sandbox` NOTIFY
  (10.4) and a full reload every 30 s, plus the catalogue in memory; a `gone` or unknown id is answered from a small
  recent-gone set or 404. The route never takes a database connection, so a Postgres blip or a busy pool never slows
  it (p99 < 1 ms in-process).
- tools refuses states other than `starting`/`ready` (its rule today, A6.F29); the gateway accepts every live state,
  so a releasing sandbox can finish its last write.

### 3.10 The supervisor's control API that hermit uses (surface `supervisor-control`, types in `supervisor-api`)

hermit calls, at `http://<sandbox address>:7070` with `Authorization: Bearer <control token>` on every route:

| Call | Answer hermit relies on | Timeout |
|---|---|---|
| `GET /v1/health` | 200 `{"ok": true, "ready": bool, "protocol": 1, "version": "…", "bootId": "…", "busy": bool}` | 2 s |
| `POST /v1/lifecycle/release` `{"reason": ReleaseReason, "code": "sandbox_released"\|"sandbox_lost", "force": bool, "deadlineMs": 30000}` | 200 `{"released": true}` once Claude Code is stopped and every FUSE write magpie accepted is stored; 409 `{"code": "sandbox_busy", "run": {"state", "turnId", "tasks"}}` when `force` is false and a turn, task or ask is open | `deadlineMs` + 5 s |
| `POST /v1/lifecycle/prepare` `{"kind": "snapshot", "force": false, "deadlineMs": 10000}` | 200 `{"ready": true}` after a `sync(2)` (before a workspace clone, 3.8) | 15 s |
| `POST /v1/exec` | supervisor 15.4; used only behind the audited admin route `POST /v1/admin/sandboxes/{id}/exec` (16) | 65 s |

- **Health.** `ok: true, ready: true` = serving. `ok: true, ready: false` = alive and starting or restarting (FUSE,
  lease or descriptor not ready yet): never unhealthy. No answer, a refused connection, 401 or `ok: false` = not
  answering. A starting sandbox becomes `ready` only on `ready: true` (11.2 step 6).
- **Release.** With `force: false` (idle, soft lifetime, pool, requested) a 409 means "not released": hermit keeps the
  sandbox and records why (13.12). With `force: true` (deleted, offboarded, not allowed, operator, recycle, hard
  lifetime) the supervisor ends a running turn cleanly (the turn is reported `canceled`; with code `sandbox_lost` it is
  reported failed) and answers 200; hermit destroys the sandbox after the 200 or at the deadline, whichever is first.
- Only hermit calls :7070. No other component holds the control token or a network path to the port (21.1).
- Pause and resume calls (`prepare pause`, `resumed`) exist in the supervisor for pause-capable implementations; the
  YYC implementation does not make them. The control port 7070 is fixed in the contract for every implementation.

### 3.11 Operator page `GET|HEAD /ops/*`

- Callers: pigeon (forwarding an admin's browser, with `Aviary-Person`), the admin token, and `/ops/snapshot` also the
  monitor token.
- For pigeon: hermit re-checks that `Aviary-Person` is an admin in the current catalogue (403 `not_admin` page
  otherwise, A0.F11, A19.F4).
- `/ops/` -> the HTML page; `/ops/snapshot?tz=<IANA zone>` -> JSON (section 18). Anything else under `/ops/` -> 404.
  Methods other than GET/HEAD -> 405.
- The page's content is implementation-specific; the contract only says it exists, is read-only, and holds no message
  contents.

### 3.12 Admin routes `/v1/admin/**` (YYC implementation; section 16)

Not used by any service. Listed in 16, including the person purge (`POST /v1/admin/people/{email}/purge`), which is
the only path by which a person's data is deleted after offboarding.

---

## 4. Sandbox state machine

### 4.1 States

```
                PUT (new)                         supervisor ready:true
   (none) ───────────────> starting ─────────────────────────────────> ready ──(soft release asked, supervisor busy:
                              │  phase: capacity -> workspace ->           │      409, stays ready)
                              │  placing -> host -> image -> creating ->   │   ^
                              │  supervisor                                │   │ resume (pause-capable implementations only)
                              │ start failed / no capacity / deadline      v   │
                              v                                       releasing ──(supervisor released or deadline,
                            gone <───────── destroyed ────────────────────┘          then sandboxd DELETE)
                              ^
                              └── died: host lost, vanished (OOM), unhealthy, migration lost it ──(directly, no releasing)
                                                                          paused (not in YYC)
```

| State | Meaning | Usable by its token (gateway liveness) | In the pool count |
|---|---|---|---|
| `starting` | being made; `phase` says where | yes (from `creating` on the booting supervisor uses it) | from `slot_claimed` on |
| `ready` | the supervisor answered `ready: true` | yes | yes |
| `paused` | (not in YYC) frozen | yes | implementation-defined |
| `releasing` | the supervisor agreed or was told to release; destroy follows | yes (last writes) | yes |
| `gone` | destroyed or never made; terminal; the row stays as history | no | no |

### 4.2 Transitions and timers (YYC values; all in `hermit.json`, section 9)

Release kinds (13.12): **soft** = the supervisor is asked with `force: false` and may refuse while busy; **forced** =
`force: true`, the running turn is ended cleanly; **lost** = no supervisor call, destroy at once.

| From | Event | To | Reason / code |
|---|---|---|---|
| — | `PUT` with no live sandbox | `starting/capacity` | |
| `starting/capacity` | slot free (live count < `maxActive` 10) | `starting/workspace` | |
| `starting/capacity` | pool full: a victim exists (ready, not busy, no busy report for 90 s, `last_active_at` older than 120 s, `reserved_until` passed, no live task claim; oldest `last_active_at` first) | victim: soft release (`pool`); retry | |
| `starting/capacity` | still full after `capacityWaitSeconds` 60 | `gone` | `pool_full` / `sandbox_busy` |
| `starting/workspace` | workspace ensured (15.5) | `starting/placing` | |
| `starting/placing` | a host slot chosen (13.3) | `starting/image` | |
| `starting/placing` | no slot: scaler launch requested | `starting/host` | |
| `starting/host` | a host became placeable | `starting/placing` | |
| `starting/host` | launch answered `full` or `off` | `gone` | `hosts_full` / `sandbox_busy` |
| `starting/host` | no host within `hostWaitSeconds` 170 | `gone` | `no_host_in_time` / `sandbox_busy` |
| `starting/image` | digest resolved | `starting/creating` | |
| `starting/creating` | sandboxd `POST /v1/sandboxes` 200 (and gateway peers pushed) | `starting/supervisor` | |
| `starting/creating` | sandboxd 4xx/5xx other than 503-busy, or 3 tries failed | `gone` | `start_failed` (note = the error class) / `sandbox_failed` |
| `starting/supervisor` | supervisor `GET /v1/health` says `ready: true` | `ready` | |
| `starting/supervisor` | no `ready: true` within `supervisorWaitSeconds` 120 | `gone` (destroyed) | `supervisor_timeout` / `sandbox_failed` |
| `starting/*` | `start_deadline` (created + `startDeadlineSeconds` 480) passed | `gone` (destroyed) | `start_timed_out` / `sandbox_failed` |
| `ready` | not busy, `last_active_at` older than `idleMinutes` 30, `reserved_until` passed | soft release -> `releasing` if the supervisor agrees | `idle` / `sandbox_released` |
| `ready` | `expires_at` (created + `maxLifetimeHours` 8) passed and not busy | soft release | `lifetime` / `sandbox_released` |
| `ready` | `hard_expires_at` (expires + `lifetimeGraceMinutes` 60) passed, busy or not | forced release (the supervisor ends the turn cleanly within 30 s) | `lifetime` / `sandbox_released` |
| `ready` | its host lost (13.7) | `gone` | `host_lost` / `sandbox_lost` |
| `ready` | its host lists it not alive or not at all, host reachable, not migrating (13.4) | `gone` (destroyed) | `vanished` / `sandbox_lost` |
| `ready` | no activity report for 90 s, gateway answering, and `GET /v1/health` not answering twice 5 s apart | `gone` (destroyed) | `unhealthy` / `sandbox_lost` |
| `ready` | migration lost it (13.10) | `gone` | `migration_failed` / `sandbox_lost` |
| `ready` | person no longer allowed (janitor) | forced release | `not_allowed` / `sandbox_released` |
| any live | magpie release `deleted`/`offboarded`, workspace delete | forced release | `deleted` \| `offboarded` / `sandbox_released` |
| any live | magpie release `requested` | soft release, retried until a report says idle | `requested` / `sandbox_released` |
| any live | operator release or recycle (16) | forced release (recycle with `onlyIdle: true`: soft) | `operator` \| `recycle` / `sandbox_released` |
| `releasing` | supervisor answered 200 (or the 30 s deadline) and sandboxd `DELETE` done | `gone` | (kept) |
| `releasing` | stuck > 10 min (sandboxd unreachable) | `gone` after a forced `DELETE` on every reachable host | `stuck` (code kept) |

Rules carried over: one live sandbox per conversation (unique index); a released or lost sandbox is replaced silently on
the next `PUT` (the model is told by the managed CLAUDE.md, not per message, A3.F11); nothing is saved on release
(`/workspace` is JuiceFS, `~/.claude` is magpie).

Changes from today, each fixing an inventory finding:
- **Idle and lifetime never cut work** (A9.F17, A16.F5): the supervisor's own view decides (a soft release it refuses
  while busy does nothing); at 9 h the release is forced but orderly (the supervisor ends the turn cleanly with
  `sandbox_released`), never a bare `runsc kill`. A question nobody answers is not work and never holds a sandbox
  (DECISIONS 17): the run is `asking`, not busy, and any release parks its turn (supervisor 7.6), which resumes in a
  new sandbox once the person answers; no turn is cut and no question times out for the person.
- **A sandbox that was just handed out is not idled out** (the `PUT` reservation, 3.6.2).
- **A sandbox that dies is reported within 10 s** (A16.F6, gap-0 #2): host heartbeat every 5 s carries each sandbox's
  `alive` flag (13.4); magpie gets `gone/sandbox_lost` on the feed and ends the turn instead of spinning.
- **No capacity is `sandbox_busy`, not a 500** (A3.F7, gap-0 #1).
- **Starts, migrations and releases survive a hermit restart** (A16.F4): sections 10.3, 11, 13.10, 13.12.
- **Idle is the supervisor's report**, not a 60 s touch from a UI process (A1.F10, A3.F1, A9.F16, A11.F16).

### 4.3 The phases the caller sees, and the chip

magpie shows "preparing the workspace" when a `PUT` answers 202 and the sandbox is not `ready` within 1 s (its rule,
today's `PREPARING_AFTER_MS`, A1.F8), and clears it on `ready` or `gone`. hermit's `phase` lets magpie (and the ops
page) say what it waits for; staff see only the chip and, after 20 s, the app's "This can take a minute or two"
(A4.F31, A14.F11, A2.F32). Stop while preparing is magpie's: it stops waiting; the sandbox keeps starting and is used by
the next send or idles out (A20.F4, as today).

---

## 5. Supervisor bootstrap identity

The question: without deployment-specific supervisor configuration, how does a supervisor learn which sandbox it is,
and where the fixed endpoints are?

### 5.1 The contract every hermit implementation must meet when it starts a sandbox

1. **The first process** of the sandbox is the image's entrypoint, `/usr/local/bin/supervisor-keeper`, as PID 1 (no
   tini; the keeper reaps and owns the stdin pipes, platform R12). hermit's implementation passes no arguments.
2. **The boot environment carries the gateway and the identity**: `http_proxy`, `https_proxy`, `HTTP_PROXY`,
   `HTTPS_PROXY` = `http://sbx:<sandbox token>@<proxy address>` (YYC: `10.202.1.1:3128`, the fabric's egress VIP) and
   `NO_PROXY` = `no_proxy` = `localhost,127.0.0.1,::1`, plus the image's own `Env` and `HOME=/home/sandbox`. The sandbox
   token (6.1) travels only as the proxy credential. Nothing else in the environment is meant for the supervisor.
3. **The fixed names are reached through that proxy**, as absolute-form requests `http://hermit/v1/sandbox/…`,
   `http://magpie/v1/sandbox/…`, `http://model/…` and `http://tools/mcp`, with `Proxy-Authorization: Basic
   base64("sbx:" + token)`. The sandbox needs no DNS and no `/etc/hosts` entries for them (today the sandbox has no
   DNS and may reach only the proxy port, A5.F34); `/etc/hosts` is the supervisor's (`localhost`, `sandbox`,
   supervisor 4.3). This is the only addressing mode (6.5).
4. **Inbound control on port 7070** from hermit only, with the control token (6.2).
5. **`/workspace`** is the conversation's workspace, writable by uid 1000, with its quota already set; `/home/sandbox/
   .claude` is an empty directory (the supervisor's FUSE mountpoint); `/dev/fuse` is available.

### 5.2 Boot sequence

```
sandboxd creates the sandbox with the boot environment of 5.1 (hermit, 11.2 step 5)
supervisor-keeper (PID 1) starts the supervisor with that environment
supervisor: GET http://hermit/v1/sandbox/self via the proxy                        retry 0.5 s doubling to 5 s, forever
   gateway: verify the token locally (6.3), forward with Aviary-Sandbox-* and its pair token
   hermit:  render the descriptor for that sandbox row (3.7.1)
supervisor: :7070 up (health ready:false), lease from magpie, FUSE ~/.claude, managed files, pre-spawn Claude Code
supervisor: health ready:true; POST http://hermit/v1/sandbox/activity {state: idle, …}   (then every 15 s)
hermit (start task): GET http://<ip>:7070/v1/health every 300 ms -> ready:true -> state ready   (11.2 step 6)
```
- Everything deployment-specific (managed CLAUDE.md, title instructions, mirror env, quotas, limits, lifecycle timers,
  secret keys) comes from the boot environment and `GET /v1/sandbox/self`. Model ids come from `GET
  http://model/v1/aviary/models` (platform R18; the supervisor's business). The supervisor build is identical in every
  deployment.
- A keeper restart of the supervisor repeats the sequence; the sandbox stays `ready` (health answers `ready: false`
  for the seconds it takes, which hermit treats as alive; a missed activity report releases nothing before 90 s of
  silence and two health checks that get no answer, 4.2).

---

## 6. Tokens and identity

### 6.1 The sandbox token (signed, self-verifying)

- Format `hst1.<kid>.<payload>.<sig>` (ASCII, URL-userinfo safe): `kid` decimal; `payload` = base64url (no padding) of
  the fixed binary `version=1 (1 byte) | sandbox id (16) | conversation id (16) | source IPv4 (4; 0.0.0.0 = none) |
  expiry (u64 big-endian Unix seconds = the row's hard_expires_at) | person (UTF-8, 1-254 bytes)`; `sig` = base64url of
  the Ed25519 signature over `"hermit-sandbox-token\0" || payload`. About 180 characters.
- Keys: `HERMIT_TOKEN_KEYS` = `kid:base64url(32 random bytes)` entries; each master key yields, by HKDF-SHA256, an
  Ed25519 signing seed (info `hermit sandbox token ed25519`) and an HMAC key for control tokens (info `hermit control
  token`). The row stores `token_kid`. Ed25519 signing is deterministic, so any replica re-derives the same token from
  the row and the key, which is what makes a start resumable (today the plaintext lived only in one replica's memory
  until create, A3.F8, A16.F4).
- It is the sandbox's only credential: the proxy, the fixed names, the mirrors, the model route, the tools MCP. It is
  usable while it verifies, has not expired, is presented from its source address, and the gateway's liveness view
  (6.3) does not say the sandbox is gone or its person not allowed.
- Inside the sandbox it is not a boundary (the sandbox user has passwordless sudo, A5.F4); the boundary is the
  gateway's source-address binding (YYC: the overlay /32, A6.F1): a token copied out of a sandbox is useless elsewhere.
- Rotation of `HERMIT_TOKEN_KEYS`: add a new key (next kid) and roll; `GET /v1/sandbox-token-keys` publishes it and
  gateways learn it on their next refresh or the first token naming it; new sandboxes use the newest key; old rows keep
  their `token_kid`; drop the old key only when no live row uses it (`select count(*) from sandboxes where status <>
  'gone' and token_kid = $old` = 0, at most 9 h). hermit refuses to start when a live row's `token_kid` is missing.

### 6.2 The control token

- Format `hct_` + base64url(`HMAC-SHA256(control key[token_kid], "control\0" + sandbox id)`). Never stored; derived
  whenever hermit calls the supervisor. The supervisor learns only its SHA-256 from `/v1/sandbox/self`.
- Holder: hermit only. It replaces today's plaintext `sandboxes.exec_token` handed to the UI (A3.F13, A5.F11). It is
  not in the `Sandbox` object and magpie never sees it.

### 6.3 What the gateway does with a sandbox request (hermit's half of `gateway-api`, platform 8.2)

For a request from a sandbox on the proxy port:
1. **Credential.** `Proxy-Authorization: Basic base64("sbx:" + token)`. Missing -> 407.
2. **Verify locally**, with no call to hermit: the token parses, its `kid` is in the cached key set (an unknown kid
   triggers one refetch of `GET /v1/sandbox-token-keys`, at most every 10 s), the Ed25519 signature is valid, the
   expiry has not passed, and the source IPv4 in the token equals the connection's source address (`::ffff:`
   stripped; skipped when the token's address is 0.0.0.0). Any failure -> 403 `not_allowed` (definitive: the token is
   not valid).
3. **Liveness (revocation).** Per sandbox id, the answer of `GET /v1/sandbox-tokens/{sandboxId}` (1 s timeout):
   - cached 30 s; when older, refreshed in the background while the cached answer keeps serving;
   - `live: false` or 404 -> 403 `not_allowed` (cached 30 s);
   - **hermit unreachable, timing out or answering 5xx**: the last answer is served whatever its age; with no answer
     ever (a gateway pod that just started), the request is admitted on the strength of step 2 and the lookup retried
     in the background. A hermit problem therefore never produces 401/403/407/5xx for a valid token: hermit is not on
     the data path of a running turn (model calls, tool calls and FUSE writes to magpie keep flowing).
   Revocation latency is at most 30 s after a sandbox goes `gone` or its person leaves the catalogue (plus the
   catalogue's own propagation, 3.4.1).
4. **Route.** `hermit` and `magpie`: only `/v1/sandbox/**`, forwarded to `http://hermit` / `http://magpie` with the
   gateway's pair bearer, `Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation`, `Aviary-Sandbox-Person` from the verified
   token and `Aviary-Request` (a new v7 id unless present); client `Authorization` and every client `Aviary-*` header
   are stripped; any other path on these names -> 403. `model`, `tools` and the internet as platform 8.2 and egress's
   own rules say.
5. **Transport.** Request and response bodies streamed both ways without buffering, NDJSON and SSE included; bodies up
   to 64 MiB (64 MiB + 1 -> 413); responses held open for long-polls of up to 30 s (idle timeout 120 s).

hermit, on `/v1/sandbox/**`, requires the gateway token and checks the three headers against the row.

### 6.4 tools

tools either trusts the gateway's owner header (platform 17.2: it accepts only the gateway's bearer) or verifies a
token itself exactly as 6.3 steps 2-3 (refusing states other than `starting`/`ready`), replacing its direct `select …
from sandboxes` (A6.F29). hermit supports both.

### 6.5 The gateway in YYC: two parts, one addressing mode

- **HTTP part: egress** (jarvis3, platform 17.2): the proxy on :3128 implementing 6.3 and platform 8.2.
- **Network part: hermit's fabric** (namespace `yyc-ai-fabric`, sections 13-14): the `hermit-gateway` DaemonSet
  installs the WireGuard hub on the gateway node, routes the sandbox overlay 10.203.0.0/16 to the AWS hosts, and DNATs
  the egress VIP `10.202.1.1:3128` to the ready egress pods' :3128 (13.8). Each sandbox's nft table admits only that
  VIP port outward and :7070 from the control range inward.
- **One addressing mode: the proxy.** There is no VIP :80, no fixed-name listener on :8081 and no `/etc/hosts`
  mapping of the names: every sandbox client (the supervisor in Rust with its proxy-aware HTTP client, Claude Code,
  which honours `HTTP(S)_PROXY` for its API and MCP traffic, pip, npm, curl) uses absolute-form requests to the proxy
  (supervisor 17.1-17.2, whose smoke test fails a Claude Code that bypasses the proxy). The platform's `:8081` listener
  and its rules go (28).

---

## 7. Per-sandbox secrets by key

- Model: a **grant** is (scope, key) -> value, where scope is the deployment, a person, or a conversation. A sandbox may
  read key K if a grant for K exists at its conversation, else its person, else the deployment scope (most specific
  wins). Values are fetched at use time by the supervisor (`GET /v1/sandbox/secrets/{key}`); the supervisor hands them
  to the agent per key when asked (supervisor 15.2) and never writes them to the transcript.
- The contract is the two routes in 3.7.2 and the descriptor's `secrets.keys`.
- **YYC implementation: no grants.** There is no grants table and no admin route to add one. `GET /v1/sandbox/secrets`
  answers `{"keys": []}`, every key 404 `not_granted`. YYC sandboxes hold no secrets: model credentials are behind
  `model`, tool credentials behind `tools` (A3.F14, A5.F18, A6.F39). Adding grants later is a new table and admin route
  in hermit only.

---
## 8. The YYC implementation: process

### 8.1 Repository and crates (DE0CH/aviary, platform 4)

```
aviary/
  crates/
    hermit-api/         lib: the contract's types (serde, camelCase; SandboxSelf, ActivityReport, ActivityReply,
                        ReleaseReason, the token format and its verifier), a typed client, `fake-hermit` (in-process
                        fake for other crates' tests) and the conformance checks (24.1) any implementation must pass
    hermit/             bin `hermit`: the YYC implementation (this spec, sections 8-23)
      src/main.rs       serve | import-jarvis | legacy-cleanup | catalogue-check (subcommands; only `serve` serves)
      src/api/          routes: deployment, people, warm, sandboxes, events, sandbox (the /v1/sandbox/** routes),
                        token_keys, workspaces, ops, admin
      src/auth.rs       caller tokens (3.2)
      src/catalogue.rs  3.4.1
      src/config.rs     hermit.json (9)
      src/db/           pool, migrations, leader, notify, claims (10.3), live map (3.9), models
      src/lifecycle/    manager (PUT/GET/release), starter (11), janitor (12), release (13.12), events feed, tokens (6)
      src/fabric/       placement, slots, scaler, launch, ec2, reconcile, heartbeat, peers, egress_targets, migrate,
                        packing (pure), images, sandboxd client (mTLS), wg keys
      src/workspace/    JuiceFS ops (15)
      src/ops/          snapshot sections, health verdict, page.html (include_str!), k8s reader
      src/drain.rs, src/metrics.rs
    sandboxd/           the host agent, moved from jarvis3 chat/sandboxd/daemon (gvisor-patches beside it;
                        tests/soak.py); its API types in `sandboxd-api`; proto/ is not moved (historical, A5.F51)
  fabric/host/          setup.sh, gateway.sh, gateway-net.sh, make-certs.sh (from chat/sandboxd/host)
  fabric/aws/           infra.sh, legacy-down.sh (from chat/sandboxd/aws)
  images/sandbox/       the sandbox image (17): Dockerfile, apt-wrapper.sh, docker-wrapper.sh, reportlab-cjk.py;
                        the supervisor binaries come from the same aviary commit
```

The `hermit` crate has exactly one sandbox provider, the fabric. A `Fabric` trait exists only so tests inject a fake
(no configuration selects it). The jarvis3 `kubernetes` and `e2b` providers, `legacyProvider`, `agent.mjs`, `/init`,
the wire proxy, `sbx-init.mjs`, `send-file-hook.mjs` and `e2b-template.mjs` are not ported (A3.F20, A3.F21, A3.F36,
A5.F16, A7.F56).

### 8.2 Dependencies

| Concern | Crate |
|---|---|
| Runtime, HTTP server | `tokio`, `axum` 0.8, `hyper` 1, `tower-http` (timeouts, request ids, body limits), `aviary-core` (auth, drain, logs, health) |
| HTTP client (magpie, egress, supervisors, sandboxd) | `reqwest` (rustls, HTTP/1.1 pool); sandboxd: one client per host with a custom CA, a client identity and `resolve("sandboxd", <wg ip>)` so certificate name `sandboxd` is checked while connecting by address |
| Postgres | `aviary-pg` (`tokio-postgres` + `deadpool-postgres`, platform R9), `postgres-types` (uuid, chrono, json, inet via `ipnetwork`) |
| EC2 | `aws-sdk-ec2` + `aws-config` with static credentials from env |
| Kubernetes reads | `kube` + `k8s-openapi` (in-cluster config; deployments, pods, endpointslices) |
| Crypto | `ring` (Ed25519 signing, HKDF, HMAC-SHA256, SHA-256, constant-time), `x25519-dalek` (WireGuard keys), `base64` |
| JSON | `serde`, `serde_json` |
| Caches | `moka` |
| Files | `rustix` (`openat2`), `tokio::fs` |
| Logs, metrics | `tracing` JSON lines through `aviary-core::log` (platform 15.1); Prometheus text exposition |

### 8.3 Ports

| Port | Serves | Exposed |
|---|---|---|
| 8080 | the whole API (sections 3, 16, 18) | Service `hermit` port 80 -> 8080 |
| 9090 | `GET /healthz`, `GET /readyz`, `GET /metrics`, `GET /version` (platform 3.2) | not in the Service; kubelet only |

### 8.4 Environment (secrets and pod facts only)

From Sealed Secret `yyc-ai/hermit` (store `yyc`, platform 12.2) unless noted. hermit refuses to start when a required
one is missing or empty and logs which (never the value).

| Variable | Content |
|---|---|
| `HERMIT_DATABASE_URL` | `postgresql://hermit:…@pg-shared.db.svc.cluster.local:5432/hermit?sslmode=require` (Cluster `pg` on the old cluster through the bridge Service, 30) (TLS required, no CA pin: CNPG's operator-issued CA is short-lived, platform 3.9/R10) |
| `HERMIT_TOKEN_KEYS` | `1:<base64url 32 bytes>[,2:…]` (6.1) |
| `HERMIT_ACCEPT_PIGEON`, `HERMIT_ACCEPT_MAGPIE`, `HERMIT_ACCEPT_GATEWAY`, `HERMIT_ACCEPT_TOOLS`, `HERMIT_ACCEPT_MONITOR`, `HERMIT_ACCEPT_ADMIN` | accepted caller tokens (3.2) |
| `HERMIT_TOKEN_MAGPIE` | hermit's token for magpie (`/v1/ops/*`, `/v1/hermit/**`) |
| `HERMIT_TOKEN_EGRESS` | hermit's token for `GET http://egress/v1/ops/summary` |
| `HERMIT_FABRIC_CA`, `HERMIT_FABRIC_CLIENT_CERT`, `HERMIT_FABRIC_CLIENT_KEY` | mTLS to sandboxd (CN `hermit`) |
| `HERMIT_FABRIC_HOST_CERT`, `HERMIT_FABRIC_HOST_KEY` | the shared AWS host server certificate, put into user data |
| `HERMIT_AWS_ACCESS_KEY_ID`, `HERMIT_AWS_SECRET_ACCESS_KEY` | IAM user `hermit-scaler` (14.4) |
| `HERMIT_JUICEFS_META_URL`, `HERMIT_JUICEFS_META_PASSWORD` | for `juicefs quota` (15) and for AWS hosts' user data (host side mount); the URL is `postgres://juicefs@<gw private IP>:30433/jfs_yyc_ai`, the one address both a pod and a host reach (30) |
| `HERMIT_POD`, `HERMIT_NAMESPACE` | fieldRefs `metadata.name`, `metadata.namespace` (not secrets) |

Nothing else: every non-secret value is in `hermit.json` (section 9). PEMs keep real newlines (no `\n` unescaping).

### 8.5 Image

`ghcr.io/de0ch/hermit:<sha12>` (public, platform R21): static musl `hermit` binary plus the `juicefs` 1.4.1 CLI
(sha256-checked at build) on `gcr.io/distroless/static-debian12:nonroot` (uid 65532), read-only root, `/tmp` an
emptyDir of 256 MiB. Built by aviary's `ci.yml` (platform 6.1).

---

## 9. YYC configuration (`hermit.json`)

The only place YYC-specific values live (besides the catalogue). ConfigMap `hermit-config` (namespace `yyc-ai`) from
`YYC/yyc-ai/deploy/hermit/` (files `hermit.json`, `system-prompt.md`, `sandbox-rules.md`, `title-system.md`),
generated **with** a name hash, so a change rolls hermit (zero downtime, section 20). Loaded once at start; validated
strictly (unknown keys are an error, so a typo fails the rollout's first pod, which never becomes ready, and the old
pods keep serving).

```jsonc
{
  "product": { "name": "YYC AI", "locale": "zh-HK" },
  "signIn": {
    "access": [ { "teamDomain": "yyc-hk.cloudflareaccess.com", "audience": "21cd5332d4d7d38dc7c97a90ec9de3b7bdf6e28098bfd49816639bb237349138" } ],
    "appScheme": "dev.de0ch.yycai"
  },
  "catalogue": "/etc/hermit/catalogue/catalogue.json",
  "prompt": { "instructions": "/etc/hermit/config/system-prompt.md", "rules": "/etc/hermit/config/sandbox-rules.md",
              "title": "/etc/hermit/config/title-system.md" },
  "sandbox": {
    "image": "ghcr.io/de0ch/hermit-sandbox@sha256:<64 hex>",   // pinned by digest; the AMI workflow follows it (14.3)
    "cpu": 1, "memory": "1Gi", "pids": 4096, "openFiles": 65536,
    "maxActive": 10, "idleMinutes": 30, "maxLifetimeHours": 8, "lifetimeGraceMinutes": 60,
    "capacityWaitSeconds": 60, "hostWaitSeconds": 170, "supervisorWaitSeconds": 120, "startDeadlineSeconds": 480,
    "poolVictimIdleSeconds": 120, "reserveSeconds": 120, "activityEverySeconds": 15,
    "workspace": { "quotaGiB": 1, "quotaInodes": 1000 },
    "tmpGiB": 5,
    "proxy": "http://sbx:{token}@{vip}:3128",          // the boot environment's proxy variables (5.1)
    "env": {                                           // the descriptor's env (3.7.1); {token} and {vip} are filled in
      "PIP_INDEX_URL": "http://sbx:{token}@{vip}:3128/mirror/pypi/simple", "PIP_TRUSTED_HOST": "{vip}",
      "npm_config_registry": "http://sbx:{token}@{vip}:3128/mirror/npm/"
    }
  },
  "fabric": {
    "sandboxCidr": "10.203.0.0/16", "hostCidr": "10.202.0.0/24", "egressVip": "10.202.1.1",
    "controlCidr": "10.0.0.0/16", "podCidr": "<pod-cidr>",   // the new cluster's, 10.210.0.0/16 (DESIGN 15.2.1 I1)
    "egressPorts": [ { "vip": 3128, "target": 3128 } ],
    "egressService": "egress",                // EndpointSlices of this Service are the gateway's targets (13.8)
    "gateway": { "name": "hermit-gw", "url": "https://10.202.0.1:7443", "endpoint": "[<gateway node IPv6>]:51821" },
    "sandboxd": "aviary-<sha12>",              // the release the AMI and the gateway install (platform 9.2, 13.5)
    "runsc": "<DE0CH/gvisor release>",
    "cpuFeatures": "3dnowprefetch,abm,adx,…,xsaveopt",   // today's mask, unchanged (config.json:24)
    "cpuOvercommit": 3, "spareSlots": 0, "minAvailableMemory": "256Mi",
    "minHostLifetimeSeconds": 900, "hostDeadSeconds": 30, "gatewayDeadSeconds": 120, "gatewaySettleSeconds": 120,
    "bootTimeoutSeconds": 480, "orphanInstanceSeconds": 600, "jfsRetireSeconds": 120,
    "migration": { "downtimeMs": 100, "maxRounds": 20, "maxDowntimeMs": 500, "idleCpu": 0.05, "minAgeSeconds": 30, "maxScratchMib": 512 },
    "aws": {
      "region": "eu-central-1", "launchTemplate": "hermit-host", "maxHosts": 3,
      "subnet": "subnet-009eab323e09d6338", "tags": { "app": "hermit-host", "hermit": "yyc-ai" },
      "fallbackHost": { "memory": "15.5Gi", "reserve": "1Gi", "cpus": 4 }
    }
  },
  "ops": {
    "timeZone": "Asia/Hong_Kong",
    "deployments": [ "pigeon", "magpie", "hermit", "egress", "tools", "cloudflared" ],
    "podNodePrefix": "yyc-pool-",
    "budgetWatch": 0.8
  }
}
```

- `fabric.aws.pruneImages` (default `false`): the image bucket's images no one uses are deleted with the EC2 reconcile
  (all but the configured image, the images of sandboxes not gone or started in the last 7 days, and objects put there
  in the last day); Jarvis 4's fabric sets it, YYC AI keeps every image.
- `aws.emptyMinutes` (accepted and unused today, A3.F28) is gone. `aws.instance` (`"ai.yyc.deyaochen.com"`, an EC2
  tag, gap-1 5.7) is replaced by the explicit `tags` map; new hosts are tagged `app=hermit-host`, `hermit=yyc-ai`,
  `Name=hermit-host-<name>` (instance and volume).
- `sandbox-rules.md` is today's `SANDBOX_RULES` text (jarvis3 `web/claude-code.ts:13-35`) moved verbatim into YYC's repo
  with `1 GiB`, `1,000 files` and `5 GiB` replaced by `{workspaceGiB}`, `{workspaceFiles}`, `{tmpGiB}`. Its "Check big
  writes" paragraph stays until Deyao changes the prompt (A3.F16). `system-prompt.md` moves unchanged (A7.F11).
  `title-system.md` is the fork's `TITLE_SYSTEM` (3.7.1).
- The `egress` Service name and the egress ops route are YYC coupling between two YYC implementations, allowed by the
  rules (section 1.1).

---

## 10. Database (`hermit`)

### 10.1 Placement

CNPG `Database` `hermit` (owner role `hermit`, managed role with password from `db/pg-role-hermit`), on cluster `db/pg`
(platform 11.1). Closed to every other role (`REVOKE CONNECT … FROM PUBLIC`). In the daily locked dump (platform
11.2). Role settings `statement_timeout = '15s'`, `idle_in_transaction_session_timeout = '60s'`. Pool: 10 connections
per replica for requests and background tasks, plus one dedicated leader connection and one dedicated LISTEN
connection (platform R11, 11.4), connect timeout 5 s, idle 30 s. Application name `hermit:<pod>`.

**No transaction is ever held across a network call** (sandboxd, EC2, the supervisor, magpie, egress, JuiceFS): every
transaction is a few statements on rows and commits in milliseconds, so the 60 s idle-in-transaction limit is never
approached and no long operation pins a pooled connection. Long operations are protected by row claims (10.3), not by
transactions. The routes that must answer during a database problem never use the pool: people and deployment facts
(memory), token keys and sandbox liveness (the in-memory live map, 3.9), and held long-polls (they wait on the
listener, and re-read with one short query when woken).

### 10.2 Schema (migration 1)

Migrations are embedded SQL files applied in order at start under `pg_advisory_lock(7300, 1)` on a dedicated
connection, each in one transaction, recorded in `schema_migrations(version int primary key, applied_at)`. A shipped
migration is never edited (A3.F50).

```sql
create table hosts (
  name text primary key,                                   -- 'hermit-gw', 'aws-<6 hex>'
  kind text not null check (kind in ('gateway','aws')),
  url text not null,                                       -- https://<wg ip>:7443
  wg_ip inet not null,
  wg_public_key text,
  instance_id text, private_ip inet,
  status text not null check (status in ('booting','active','retiring','gone')),
  retire_by text check (retire_by in ('operator','system')),
  max_sandboxes int check (max_sandboxes >= 0),            -- operator cap; null = by memory and CPU
  mem_total bigint, mem_reserve bigint, cpus int,
  info jsonb,                                              -- last GET /v1/host (13.4)
  created_at timestamptz not null default now(), last_seen_at timestamptz, empty_since timestamptz,
  gone_at timestamptz, gone_reason text
);
create unique index hosts_wg_ip_live on hosts (wg_ip) where status <> 'gone';

create table sandboxes (
  id uuid primary key,
  conversation_id uuid not null,
  person text not null,
  status text not null check (status in ('starting','ready','paused','releasing','gone')),
  phase text check (phase in ('capacity','workspace','placing','host','image','creating','supervisor')),
  version int not null default 1,
  name text not null unique,                               -- 'sbx-' || first 16 hex of the id
  token_kid smallint,                                      -- null only on imported history rows
  slot_claimed boolean not null default false,
  host text, ip inet,                                     -- no foreign key: imported history names old hosts
  image text, cpu real not null, memory_bytes bigint not null,
  busy boolean not null default false, busy_turn text, background_tasks int not null default 0,
  supervisor_version text, supervisor_protocol int, boot_id text,
  task text check (task in ('start','migrate','release')), -- the row claim (10.3)
  task_owner text, task_until timestamptz,
  migrating_to text, migration_started_at timestamptz,     -- set before a live migration's send (13.10)
  start_deadline timestamptz not null,
  reserved_until timestamptz,                              -- set by PUT (3.6.2)
  release_pending jsonb,                                   -- {reason, note, code, force, deadlineAt, askedAt, refusals}
  created_at timestamptz not null default now(), ready_at timestamptz,
  last_active_at timestamptz not null default now(), last_heartbeat_at timestamptz, last_busy_at timestamptz,
  expires_at timestamptz not null, hard_expires_at timestamptz not null,
  released_at timestamptz, release_reason text, release_note text, release_code text, release_message text,
  imported boolean not null default false
);
create unique index sandboxes_one_live on sandboxes (conversation_id) where status <> 'gone';
create unique index sandboxes_ip_live on sandboxes (ip) where status <> 'gone' and ip is not null;
create index sandboxes_live on sandboxes (status, created_at) where status <> 'gone';
create index sandboxes_claims on sandboxes (task_until) where task is not null;
create index sandboxes_person on sandboxes (person, created_at desc);
create index sandboxes_released on sandboxes (released_at) where status = 'gone';

create table sandbox_events (                              -- the feed (3.6.5)
  seq bigserial primary key, at timestamptz not null default now(),
  sandbox_id uuid not null, conversation_id uuid not null, person text not null,
  state text not null, phase text, version int not null, busy boolean not null,
  release jsonb
);
create index sandbox_events_at on sandbox_events (at);

create table sandbox_starts (                              -- today's ops_events kind sandbox_start (A3.F48)
  id bigserial primary key, at timestamptz not null default now(),
  ok boolean not null, ms int not null, start_ms int,
  sandbox_id uuid, conversation_id uuid, person text, host text,
  reason text, error text,                                 -- error redacted, <= 300 chars
  source_id bigint unique                                  -- the imported ops_events id (22.3)
);
create index sandbox_starts_at on sandbox_starts (at);

create table host_events (                                 -- today's vm_events (A3.F49)
  id bigserial primary key, at timestamptz not null default now(),
  kind text not null, host text, sandbox_id uuid, data jsonb not null default '{}',
  source_id bigint unique
);
create index host_events_at on host_events (at);

create table sandbox_logs (                                -- console tails (platform 15.4)
  sandbox_id uuid primary key, captured_at timestamptz not null default now(),
  bytes bytea, reason text
);

create table demand_hints (person text primary key, at timestamptz not null);       -- warm (3.5)

create table controls (                                    -- operator switches (16)
  id int primary key check (id = 1),
  sandboxes_enabled boolean not null default true, disabled_reason text,
  fresh_host_until timestamptz,
  updated_at timestamptz not null default now(), updated_by text
);
insert into controls (id) values (1);

create table admin_events (                                -- audit of every admin call (16), kept for good
  id bigserial primary key, at timestamptz not null default now(),
  actor text not null, action text not null, target text, data jsonb not null default '{}'
);
```

`host_events.kind` values: `launch`, `launch-failed`, `active`, `terminate`, `host-lost`, `migration-start`,
`migration`, `migration-skipped`, `migration-failed`, `migration-adopted`, `pack`, `retire`, `reactivate`,
`jfs-unmounted`, `jfs-mounted`, `jfs-retire`, `orphan`, `memory-kill`, `release-refused` (the legacy `drain` is not
carried over; imported rows keep their kinds).

### 10.3 Locks, claims and leadership

**Advisory locks** (`classid 7300`; all hermit locks live in database `hermit`, so they cannot collide with jarvis3's
4242xxx keys). Each transaction-scoped lock guards a few statements and is never held across I/O.

| objid | Kind | Use |
|---|---|---|
| 1 | session, start | migrations |
| 2 | transaction | capacity claim (`maxActive`) |
| 3 | session, leader | janitor (every 30 s) |
| 4 | session, leader | scaler (every 5 s) |
| 5 | transaction | placement (host + ip) |
| 7 | transaction | host launch (the `booting` row is inserted and committed; RunInstances runs after the commit) |
| 8 | transaction | event serialisation (10.5) |

**Row claims** (per sandbox: one long task at a time). `task` = `start` | `migrate` | `release`, `task_owner` = pod,
`task_until` = now + 15 s, renewed every 5 s by the replica running it (`update … where id = $1 and task_owner = $me`;
0 rows = the claim was lost: the task stops at its next step). Taking a claim is one statement: `update sandboxes set
task = $t, task_owner = $me, task_until = now() + '15 s' where id = $1 and (task is null or task_until < now()) returning
…`. Every replica runs a **takeover sweep** every 2 s: claim one row whose `task` is set and `task_until` passed (`for
update skip locked limit 1`) and resume that task from the row (11.2, 13.10, 13.12). So a crash, SIGKILL or drain of
the replica doing a task costs at most about 17 s, and no task ever depends on one replica's memory.

Rules: the janitor and the scaler never touch a row with a live claim of another task except to record intent: a
release asked for a row being started or migrated sets `release_pending` and the running task performs it when it ends
(a start that reaches `ready` with a forced release pending releases at once; a migration releases after it settles).
Destroy runs only under the `release` claim (or inside a start or migrate task's own failure path).

**Leadership:** each replica runs `lead(objid)` on its dedicated leader connection: `pg_try_advisory_lock`; if it fails,
retry every 5 s; once held, run the loop; a lost connection drops the lock and the loop stops at once (checked before
every pass with `select 1` on that connection). Postgres `tcp_keepalives` (60/10/6) free a dead peer's locks within
about 2 min; a draining replica unlocks explicitly (20.2), so a handover takes at most 5 s (A3.F9, A7.F21).

### 10.4 Internal wake-ups

Every change to a `sandboxes` row's visible fields goes through one function `bump(tx, id, changes)` that increments
`version`, inserts the `sandbox_events` row and runs `pg_notify('hermit_sandbox', '<sandbox id>')`. Each replica holds
one listener connection; held `GET …?waitMs` and `GET /v1/sandbox-events?waitMs` requests and the in-memory live map
(3.9) wake on it. A listener reconnect re-checks every held request and reloads the live map. Never exposed outside
hermit.

### 10.5 Feed reads without gaps

A bigserial can commit out of order (seq 11 visible before 10), and a reader that saw 11 would skip 10 forever. So
events are inserted only by `bump`, which first takes `pg_advisory_xact_lock(7300, 8)` and inserts the event as the
last statement before commit: event inserts are serialised and commit in seq order. At under 10 events per second the
lock costs nothing measurable. The feed then reads plainly `where seq > $after order by seq limit $limit`.

### 10.6 Retention (janitor, daily at 03:00 Hong Kong time, leader only)

| Table | Kept |
|---|---|
| `sandboxes` (`gone`) | 400 days (ops history, per-person sandbox counts, egress backfill) |
| `sandbox_events` | 7 days |
| `sandbox_starts` | 30 days (as today, A3.F10) |
| `host_events` | 400 days (today: never purged; 428 rows live) |
| `hosts` (`gone`) | 400 days |
| `sandbox_logs` | 14 days |
| `demand_hints` | 1 h (the scaler also prunes) |
| `admin_events` | forever (audit) |

(A17.F15: every hermit table now has an explicit retention.)

---

## 11. Starts (resumable)

### 11.1 Insert (in the `PUT` request)

In one transaction: insert the row `status='starting', phase='capacity', task='start', task_owner=<pod>,
task_until=now()+15 s, start_deadline=now()+startDeadlineSeconds, expires_at=now()+maxLifetimeHours,
hard_expires_at=expires_at+grace, reserved_until=now()+reserveSeconds, name='sbx-'||left(replace(id,'-',''),16),
token_kid=<newest>, cpu, memory_bytes`, plus `bump`. A unique violation on `sandboxes_one_live` (another replica raced)
-> re-read and answer with the existing row. Then the replica spawns the start task and answers 202.

### 11.2 The start task (any replica can run it)

The task holds the row's `start` claim (10.3) and renews it every 5 s; another replica's takeover sweep resumes a start
whose claim lapsed. Each step is idempotent, records its phase with `bump`, and makes its network calls outside any
transaction:

1. **capacity** (unless `slot_claimed`): under lock 7300/2: `count(*) where status<>'gone' and slot_claimed` <
   `maxActive` -> `slot_claimed=true`. Else pick a pool victim (4.2's rule: `ready`, not busy, no busy report for 90 s,
   `last_active_at < now()-120 s`, `reserved_until` passed, no live claim, another conversation, oldest first) and ask
   for its soft release (`pool`); a victim whose supervisor refuses (409) is skipped for 60 s and the next is tried; no
   victim: wait 2 s and retry until `created_at + capacityWaitSeconds`, then `gone` (`pool_full`, `sandbox_busy`).
   Waiting rows are served oldest first (a row only claims if no older unclaimed `starting` row exists).
   A person's own number (`sandbox.maxActiveByPerson`: an address or `*` to a number; absent = none): a row whose
   person already holds that many live claimed sandboxes does not claim, counts as no older row for the others, takes
   only that person's sandboxes as victims, and ends the same way (`pool_full`, `sandbox_busy`).
2. **workspace**: ensure workspace `<conversation id>` (15.5). A failure on this replica's mount (15.6) hands the start
   to another replica (the claim is released with `task_until = now()`, at most 3 hand-overs); other failures retry 3
   times 2 s apart, then `gone` (`workspace_unavailable`, `sandbox_failed`).
3. **placing** (unless `host` set): under lock 7300/5 pick a slot (13.3) and the lowest free address in `sandboxCidr`
   from `.10` (skipping `.0`/`.255` of every /24), write `host`, `ip`. No slot -> phase `host`, request a launch (13.5),
   poll every 3 s up to `hostWaitSeconds`; launch `full`/`off` -> `gone` (`hosts_full`, `sandbox_busy`).
4. **image**: resolve the digest (13.14).
5. **creating**: sandboxd `POST /v1/sandboxes` on `host` (15 min timeout) with spec `{name, ip, image, imageRef, env:
   <the boot environment of 5.1 with the derived token>, cpu, memory, pids, cpuFeatures, workspace: <conversation id>}`.
   Same name and ip already running -> treated as success (sandboxd is idempotent). 503 `busy` -> retry every 3 s
   within the deadline; 409 ip in use (a stale sandbox) -> destroy that name and retry once; other errors -> `gone`
   (`start_failed`, note = the error class, `sandbox_failed`) after destroying. On an AWS host: push the gateway's peers
   (13.8) so the /32 is routed.
6. **supervisor**: `GET http://<ip>:7070/v1/health` (control token) every 300 ms up to `supervisorWaitSeconds`;
   `ready: true` -> `status='ready', phase=null, ready_at=now(), last_active_at=now(), supervisor_*`, clear the claim,
   `bump`, record a `sandbox_starts` row `{ok, ms = now - created_at (includes the pool wait), start_ms = now - (step 5
   start)}`. If a forced release became pending meanwhile (the conversation was deleted), release at once (13.12).

Any failure path records `sandbox_starts {ok:false, reason, error}` (A3.F48) and, for anything already created, destroys
it before `gone` (13.12). A start that crosses `start_deadline` is ended by its task (or the janitor) with
`start_timed_out`.

### 11.3 What the caller sees

`PUT` 202 within 50 ms; magpie long-polls; phases tick through; `ready` arrives. Typical times (from live data,
A20.F22): warm host 1.9 s median; cold (no host) about 80 s (host boot 45 s + place 5 s + ready 9 s + first reply);
stale AMI adds about 45 s of image fetch. The overall bound is 8 min (`startDeadlineSeconds`), above the worst live
start (205 s).

---

## 12. Janitor (leader, every 30 s)

1. **Idle**: `ready`, not busy, `last_active_at < now() - idleMinutes`, `reserved_until` passed (or null), no live claim,
   no pending release -> soft release `idle` (13.12).
2. **Lifetime**: `ready`, `expires_at < now()`, not busy -> soft release `lifetime`; `hard_expires_at < now()` -> forced
   release `lifetime` regardless of busy.
3. **Silence**: `ready`, no live claim, `last_heartbeat_at < now() - 90 s` (or null and `ready_at < now() - 90 s`), and
   the gateway host has answered every heartbeat for the last `gatewaySettleSeconds` (120 s): two health checks 5 s
   apart; any answer with `ok: true` (ready or not) means alive: the sandbox is kept and `supervisor not reporting` is
   logged (the ops page shows it). Only two checks that get no answer (or `ok: false`) -> lost `unhealthy`
   (`sandbox_lost`). The same check runs at once, in the background, for a sandbox a `PUT` found silent for 45 s
   (3.6.2). While the gateway is down, or within 120 s of its return, the silence rule does not run at all: the
   supervisors cannot reach hermit and hermit cannot reach :7070, so silence proves nothing (13.7, 14.7).
   `sandbox.silenceSeconds` (optional, positive) replaces the 90 s, and half of it the `PUT`'s 45 s: Jarvis 4's fabric
   hermit, whose sandboxes report to another hermit and whose final snapshot stops the supervisor, sets it past any
   sandbox's life. YYC AI names none.
4. **Not allowed**: live rows whose person the catalogue no longer allows -> forced release `not_allowed` (A17.F17).
5. **Pending releases**: retry soft releases that are due (13.12); finish forced ones whose claim lapsed (the takeover
   sweep normally does it first).
6. **Stuck starts**: `starting` past `start_deadline` with no live claim -> destroy, `gone` `start_timed_out`.
7. **Stuck releases**: `releasing` for more than 10 min (sandboxd unreachable throughout) -> `DELETE` on its host and on
   every reachable host listing the name, then `gone` `stuck`.
8. **Orphans**: names listed by any reachable host with no non-gone row, older than 60 s (sandboxd `createdAt`) ->
   destroy, `host_events` `orphan` row (A3.F10). A name listed on the `migrating_to` host of a live row is not an
   orphan.
9. **Image recycle** (only when an operator asked for it, 16): not automatic.
10. **Retention** (10.6) once a day.

Vanished sandboxes (a ready row its reachable host no longer lists, or lists as not alive) are handled by the scaler's
heartbeat every 5 s, not here (13.4), so a dead sandbox is reported within 10 s instead of 30 s.

## 13. The fabric (YYC: gVisor on AWS behind a WireGuard gateway)

A port of jarvis3 `web/sandbox-vm.ts`, `web/packing.ts`, `web/vm-clients.ts` with the changes named in each item.

### 13.1 Layout and addresses

| Thing | Value | Today (old fabric) |
|---|---|---|
| Gateway (hub) | sandboxd in gateway mode as the main process of the host-network pod `hermit-gateway` (DaemonSet in namespace `yyc-ai-fabric`, 14.2) on one new-cluster Talos node in fsn1 labelled `hermit.yyc/gateway=true`, private address `<gw private IP>` in network `yyc`; name `hermit-gw`; overlay 10.202.0.1; WireGuard udp 51821 over IPv6 | `yyc-sbx-gw` on `yyc-pool-apps-worker2` (10.0.0.6), 10.201.0.1, installed by hand |
| Host overlay | 10.202.0.0/24 (gateway .1, AWS hosts .10-.250) | 10.201.0.0/24 |
| Egress VIP | 10.202.1.1, port 3128 only (the proxy, mirrors, the fixed names) | 10.201.1.1:3128 |
| Sandboxes | 10.203.0.0/16, a fixed /32 each that follows it | 10.200.0.0/16 |
| Hetzner network route | 10.202.0.0/15 -> `<gw private IP>` | 10.200.0.0/15 -> 10.0.0.6 (old cluster) |
| JuiceFS metadata for hosts | `<gw private IP>:30433` (NodePort of `db/pg-shared`, forwarded to `pg`, 30) | 10.0.0.2:30432 (`pg-hosts`) |
| AWS | eu-central-1, template `hermit-host`, tags `app=hermit-host`, `hermit=yyc-ai`; m7i.xlarge (4 vCPU, 16 GiB) | template `yyc-ai-sandbox-host`, tags `app=yyc-ai-sandbox`, `jarvis=ai.yyc.deyaochen.com` |

Everything is new beside the old fabric, so both run during the switch without sharing a hub, a peer set, an address
range, an EC2 tag or a launch template (section 22). After the switch the old one is removed and these become the only
values.

### 13.2 sandboxd changes (released in `aviary-<sha12>`, platform R16)

sandboxd moves unchanged in design (fixed memory budget, mTLS API, per-name locks, state files, live migration, relay;
A5.F19-F50) with these changes:

1. **First process from the image.** The bundle's `process.args` = the image config's `Entrypoint` + `Cmd` (read from
   the unpacked image's `config.json`, like `image_env`), instead of the hard-coded `tini -- node /opt/agent/sbx-init.mjs`.
   Spec field `args` does not exist; an image without an entrypoint is refused at create (400). cwd `/work`, uid 0 as
   today (the keeper and supervisor run as root and start Claude Code as uid 1000).
2. **No `~/.claude` bind.** The spec field `user` is removed (unknown spec fields are a 400, so an old controller cannot
   accidentally ask for it); `storage.rs` no longer makes `users/` folders or binds `users/<key>/claude`, and no longer
   chowns `ws/` or `users/` (15.4). The pre-made upper dir `/home/sandbox/.claude` stays (the supervisor's FUSE
   mountpoint, A5.F30).
3. **No quota setting.** hermit sets the workspace quota (15.5); sandboxd only binds `ws/<workspace>` (it still refuses
   with 503 when JuiceFS is not mounted, and with 409 `workspace missing` when `ws/<workspace>` does not exist, so a
   sandbox can never create an unquota'd folder, A18.F2).
4. **No name mapping.** No `/etc/hosts` bind: the sandbox reaches the fixed names only through the proxy (6.5), and the
   supervisor writes `/etc/hosts` itself (`localhost`, `sandbox`).
5. **Network unchanged in shape.** The per-sandbox nft table accepts `saddr <ip> -> egressVip tcp dport 3128` and the
   replies, plus `controlCidr` :7070 both ways (as today). The gateway DNATs `VIP:3128 -> target:3128` with `numgen
   random mod N` over the targets, source kept. `host.json` keeps `egressPorts` (one pair); `PUT /v1/egress {targets}`
   is unchanged.
6. **Liveness per sandbox.** `Listed` gains `alive: bool` = `populated 1` in `/sys/fs/cgroup/sbx/<name>/cgroup.events`
   (read in the 10 s CPU sampler; no `runsc` call). A sandbox whose sentry was OOM-killed by the host memcg shows
   `alive:false` within 10 s. Also `alive:false` at once (not at the next sample) for a source whose send's checkpoint
   stopped it; a missing `cgroup.events` under a present `/sys/fs/cgroup/sbx` is not alive, while an unreadable one
   on a host without that parent cgroup reports no change (logged once), so an unknown cgroup layout never reads as
   every sandbox dead (WP-19 review, 2026-11-02).
7. **FUSE.** The bundle adds the device `/dev/fuse` (`c 10:229`, mode 0666) to `linux.devices` and the cgroup device
   allow-list, and runsc runs with FUSE enabled (`--fuse` added to the global flags if the gVisor fork release still
   gates it; the flags must stay identical on every host, A5.F33). Acceptance: 24.3 (mount, write-through, live
   migration with a live FUSE mount).
8. **Names.** The controller certificate CN checked by sandboxd becomes `hermit` (was `sandbox-controller`).
9. **pids** from the spec (`pids`, default 4096) instead of the constant.
10. **Console tails.** `GET /v1/sandboxes/{n}/log?tail=65536` (bytes, `text/plain`, at most 64 KiB of
    `/var/log/sbx/<name>.log`; also served for 10 min after the sandbox ended), for hermit's `sandbox_logs`
    (platform 15.4).
11. **Migration status.** `GET /v1/sandboxes/{n}/receive` (without `wait`) answers the receive's current state
    (`receiving` | `running` | `failed` | `none`) at once, so a replica that takes over a migration can ask where it
    stands (13.10).

Everything else (TLS 1.3 only, 16 connections, 24 sandboxes per host, 4 migrations, 64 KiB bodies, the 31.5 MiB RSS
bound and its soak gate, A5.F43, A20.F13) is unchanged.

### 13.3 Slots and placement

- `slots(h) = min(floor((mem_total - mem_reserve) / sandbox.memory), floor(cpus * cpuOvercommit / sandbox.cpu),
  max_sandboxes ?? inf)`; the gateway reports all memory reserved, so it has 0 (A3.F33). m7i.xlarge: min(14, 12) = 12.
- `units(h)` = rows on the host (`status <> 'gone'`), plus rows whose `migrating_to` is the host, each `ceil(memory_bytes
  / sandbox.memory)`.
- `free(h)` = 0 under memory pressure (`memAvailable < minAvailableMemory` 256 MiB), else `max(0, slots - units)`.
- placeable = `active`, seen within `hostDeadSeconds`, `jfsMounted != false`, not `retiring`, and not excluded by
  `controls.fresh_host_until` (16: while set, only hosts created after the switch was set are placeable).
- `pick`: the fullest placeable host with `free >= need` (tie by name), excluding the source on moves. (The "hub first"
  rule is gone: the gateway runs no sandboxes.)

### 13.4 Scaler pass (leader, every 5 s)

1. **Gateway row**: upsert `hosts` for `fabric.gateway` (kind `gateway`); a row with another name at the same `wg_ip`
   goes `gone` (`replaced by <name>`).
2. **Heartbeats**: `GET /v1/host` on every non-gone host in parallel (5 s). Store `mem_total`, `mem_reserve`, `cpus`,
   `wg_public_key`, `info` (gateway, publicIpv6, wgPort, sandboxes[], cpuFeatures, images, bootId, gvisorRelease,
   memAvailable, loadAvg, uptimeS, peers, egressTargets, jfsMounted, jfsUnmountedSince, sandboxd memory), `last_seen_at`.
   A `booting` host becomes `active` once it answers with `jfsMounted != false` (event `active`, image warm-up 13.14).
   A host missing a CPU feature of `fabric.cpuFeatures` -> `retiring` (`retire_by='system'`, event `retire`).
3. **Vanished sandboxes** (new placement of A3.F10's rule): for every reachable host, each of its `ready` rows with no
   live claim and no `migrating_to` that is not in its `sandboxes[]`, or listed with `alive:false`, -> destroy, lost
   `vanished` (`sandbox_lost`). A row with `migrating_to` set is never judged from its source alone: the migration task
   (or its takeover) asks both hosts (13.10). A host not answering contributes nothing here (13.7 decides).
4. **Host loss**: the gateway silent > `gatewayDeadSeconds` 120 -> `host-lost` for it (its sandboxes: none). AWS hosts
   are judged only after the gateway has answered for 30 s straight; then silent > `hostDeadSeconds` 30 -> lost (13.7).
   Without the gateway answering, stop the pass here (nothing else can be trusted).
5. **JuiceFS gating** (A3.F25, A18.F21): an active host reporting `jfsMounted:false` -> event `jfs-unmounted` (once),
   no placement; mounted again -> `jfs-mounted`; an AWS host unmounted > `jfsRetireSeconds` 120 -> `retiring`
   (`system`, event `jfs-retire`), its sandboxes move off. A booting host without its mount after
   `bootTimeoutSeconds` 480 -> lost (`JuiceFS not mounted`); not answering after 480 s -> lost (`did not come up`).
6. **Network**: push the gateway's peers and every AWS host's mesh peers, and the egress targets (13.8).
7. **Images**: resolve the configured digest (13.14).
8. **EC2 reconcile** (13.9).
9. **Bookkeeping**: `empty_since` per host; delete `demand_hints` older than 1 h; `launch('scaler')`, plus
   `launch(transient)` while a retiring host still has sandboxes.
10. **Release one empty host** (13.6) and end the pass if one was terminated.
11. **Move off retiring hosts**: one `ready` sandbox (longest idle first) to `pick()` if movable (13.10), then end the
    pass.
12. **Packing**: `planPacking` over active, fresh, non-retiring hosts; at most one movable migration per pass (events
    `pack` + `migration-start`).

At most one terminate and one migration per pass, as today (A3.F24, A7.F23). Migrations run as `migrate` tasks (13.10)
outside the pass.

### 13.5 Capacity planning and launch

- `plan()`: `available` = free slots on placeable hosts; `pending` = booting AWS hosts x `perHost` (`perHost` from the
  latest AWS host with a known size, else `aws.fallbackHost`: 15.5 GiB, 1 GiB reserve, 4 cpus -> 12); `waiting` =
  `starting` rows in phase `host`; `demand` = `demand_hints` younger than 120 s (one per person); `need = spareSlots +
  demand + waiting` (+ sandboxes on retiring hosts when transient); `want = need - (available + pending)` (A3.F27).
- `launch(reason, transient)`: under lock 7300/7 decide and insert the `booting` rows, commit; then for each,
  `RunInstances`. Starts `min(ceil(want / perHost), cap - count)` hosts, `cap = aws.maxHosts` (+1 only when transient
  and a host is retiring). Answers `launched` / `planned` (pending covers it) / `full` (at the cap) / `off` (`maxHosts`
  0 or sandboxes disabled).
- `launchOne`: needs the gateway's WireGuard public key (from its heartbeat). Allocates `wg_ip` (10.202.0.10-.250),
  makes an x25519 key pair and the name `aws-<6 hex>`, **inserts the `booting` row before RunInstances**, then
  `RunInstances` (launch template `hermit-host` `$Default`, MinCount=MaxCount=1, tags on instance and volume) with user
  data:
  ```json
  { "host": { "name": "aws-3f1c2a", "role": "burst", "wgAddress": "10.202.0.37", "hostCidr": "10.202.0.0/24",
              "controlCidr": "10.0.0.0/16", "sandboxCidr": "10.203.0.0/16", "egressVip": "10.202.1.1",
              "egressPorts": [[3128,3128]], "reserveMemory": 1073741824,
              "imageSource": "http://10.202.0.1:7444",
              "hubPeer": { "publicKey": "<gateway wg key>", "endpoint": "[<gateway IPv6>]:51821", "allowedIps": ["10.202.0.0/24","10.0.0.0/16","10.202.1.1/32"] } },
    "wgPrivateKey": "<base64>", "tls": { "ca": "<pem>", "cert": "<pem>", "key": "<pem>" },
    "juicefs": { "metaUrl": "postgres://juicefs@<gw private IP>:30433/jfs_yyc_ai", "metaPassword": "<from env>",
                 "rootSquash": "1000:1000" } }
  ```
  User data is at most 16 KiB (EC2's limit); today's is about 6 KiB. The private key exists only there and in the host;
  the host later reports its public key, which overwrites the row's (A3.F58). Failure -> row `gone`, event
  `launch-failed` with the AWS error code (e.g. `VcpuLimitExceeded`).

### 13.6 AWS host release

An AWS host with no non-gone sandbox rows (and none migrating to it), not booting, older than `minHostLifetimeSeconds`
900 from launch -> `TerminateInstances`, row `gone` (`empty`, or `<status>, empty`), event `terminate {instance,
reason, upMs}`. Nothing else keeps a host (A3.F28).

### 13.7 Host loss

Every non-gone sandbox on the host (or migrating to it with the source also gone) -> `gone` (`host_lost`,
`sandbox_lost`) with `bump` (so magpie ends their turns within one feed poll); event `host-lost {reason, sandboxes}`; an
AWS host is then terminated (`gone` `lost: <reason>`, event `terminate`). The next `PUT` makes a fresh sandbox;
`/workspace` survives on JuiceFS and the conversation in magpie (A3.F26, A16.F6). **The gateway lost**: AWS hosts are
not judged (step 4), sandboxes keep running, the janitor's silence rule pauses (12 step 3), but every sandbox has lost
its egress and control path until the gateway returns (a single point of failure, A5.F46; the zero-downtime gap and its
maintenance runbook are 14.7). The ops verdict says so.

### 13.8 Network pushes

- **Gateway peers** (`PUT /v1/peers` on the gateway): one peer per non-gone AWS host with a key: `{publicKey,
  endpoint: "[publicIpv6]:wgPort" if known, allowedIps: [wg_ip/32, each of its sandboxes' ip/32]}` (at most 32 peers,
  A5.F37). A sandbox's /32 is listed under its `host`, and under `migrating_to` only after the move is committed.
  Pushed in each pass and right after every placement and every committed migration (after the commit, never inside a
  transaction); the push is idempotent (it sets the whole set from the rows).
- **Mesh peers** (`PUT /v1/peers` on each fresh non-booting AWS host): the other AWS hosts at `<private_ip>:51820`
  (AWS-to-AWS migrations).
- **Egress targets** (`PUT /v1/egress` on the gateway): the IPv4 addresses of the **ready** endpoints in the
  EndpointSlices of Service `fabric.egressService` (`egress`) in hermit's namespace, read with a watch (k8s
  `discovery.k8s.io/v1`, `kubernetes.io/service-name=egress`), port 3128. A terminating or unready egress pod leaves the
  list at once, which is exactly today's "stop announcing on SIGTERM" (A6.F25). With no ready endpoints the last list is
  kept (and the ops page shows a problem). Replaces `kv egress_pod:*` (A3.F51, A13.F8): egress no longer writes
  anything for hermit.

### 13.9 EC2 reconcile (every pass, at most once per 30 s)

`DescribeInstances` filtered by the `aws.tags` and states pending/running/stopping/stopped/shutting-down. An instance
unknown to `hosts` and older than `orphanInstanceSeconds` 600 -> `TerminateInstances` (event `terminate`, `orphan`). A
live row whose instance is missing and older than 2 min -> lost. Fills `private_ip` when missing (A3.F29). The tag
filter is hermit's own, so the old fabric's instances are never touched (22.2).

### 13.10 Live migration (a resumable `migrate` task)

`movable(s, from, to)`: `ready`, no live claim, no pending release, older than `minAgeSeconds` 30, the same
`gvisorRelease` on both hosts, the target has the image digest or can fetch it, and at most `maxScratchMib` 512 in use
on its scratch (the source's `scratchUsed`; not reported: no bound; 0 = no bound). Busy sandboxes may move (as today,
mid-turn); the supervisor's connections survive like the agent's did (A5.F49, A16.F7). The scratch bound is there
because the final scratch delta compares contents while the sandbox is stopped (sandboxd's `rsync --checksum`, the
WP-57 short read): about 3.2 ms per MiB in use, so 512 MiB adds about 1.7 s to the pause and a full 5 GiB scratch
would add 15.5-15.8 s (measured, WP-19 `crates/sandboxd/tests/real-host/`). **Open, for the owner of 13 before WP-40
runs packing on real hosts:** keep this bound (a sandbox above it stays where it is: packing skips it, a retiring
host waits for it to end) or narrow the checksum (e.g. the supervisor's journal via supervisor 9.9's `pwrite`
fallback plus small or recently changed files; a narrowing must pass `mig.sh`'s mapped-counter check, 0 faults). Until
it is decided, the bound stands (WP-19 review, 2026-11-02). Every sandbox now runs on AWS
(the gateway runs none), so all moves are within one cloud and region; the cross-cloud rule (`idleCpu` 0.05,
`maxDowntimeMs` 500) is kept in the code and configuration for a future second region and applies only when the two
hosts' `kind` or region differ.

The scaler starts a migration by taking the row's `migrate` claim and handing the task to the replica's task set. The
task renews the claim every 5 s; every network call runs outside a transaction:
1. **Intent.** One short transaction: `migrating_to = <to>`, `migration_started_at = now()`, event `migration-start
   {from, to, reason}`. From here on the scaler's vanished check and the orphan sweep leave the sandbox alone on both
   hosts (13.4 step 3, 12 step 8), and the target's slot is counted (13.3).
2. `GET source /v1/sandboxes/<name>/spec`.
3. `POST target /v1/sandboxes/<name>/receive {spec}` -> `{port, address}`.
4. `POST source …/send {to: "<address>:<port>", downtimeMs 100, maxRounds 20, maxDowntimeMs 500}` (15 min). 409 = not
   moved: target `DELETE`, clear `migrating_to`, event `migration-skipped`.
5. `GET target …/receive?wait=90000`: `running` -> one transaction: `host = to`, `migrating_to = null`, event
   `migration {from, to, reason, ms, final_ms, restore_ms, rounds, bytes, pause_ms}`, `bump`; commit; then push the
   gateway's peers (13.8).
6. Otherwise **settle by asking both hosts** (`GET /v1/sandboxes` on each): the source lists it alive and not
   `sending` -> target `DELETE`, clear `migrating_to`, event `migration-failed {kept: true}`; else the target lists it
   alive -> adopt it (`host = to`, as step 5, event `migration-adopted`); else the source lists it `sending` (its send
   still runs and may still switch: from its first scratch round until its entry goes) or the target still lists it
   `receiving` -> ask again; else -> lost `migration_failed` (`sandbox_lost`), event `migration-failed {kept:
   false}`. sandboxd bounds both: a receive ends restored or failed (the relay's 900 s, then 300 s for the restore after
   End), and a send against an ended receive fails within its rsync and checkpoint timeouts. As a backstop, once
   `migration_started_at` is 25 min old and the target does not list it alive, the settle deletes the target's receive
   and then decides from the source alone (alive, even if still sending: kept; else lost), events with `overdue:
   true`.
7. Clear the claim. A release that became pending during the migration runs now (13.12).

**Takeover.** A replica that dies or drains mid-migration leaves the claim to lapse (a drain lapses it at once,
`task_until = now()`); sandboxd keeps sending on its own. The replica that takes the claim over resumes from the row:
with `migrating_to` set, it asks the target `GET …/receive` (13.2 item 11): `receiving` -> wait with `?wait=90000` (step
5); `running` -> step 5; `failed` or `none` -> step 6. So a sandbox that finished moving while no replica watched is
adopted on the target, never declared vanished from its source, and the target's copy is never swept as an orphan.

The id, IP, tokens, processes, the supervisor, its FUSE mount, its open connections (supervisor -> gateway, Claude
Code -> gateway) and `/workspace` writes (committed at the source's `runsc delete`, no JuiceFS writeback, A18.F22)
survive. Measured pauses: 84-580 ms before the final scratch delta compared contents (A20.F22); with it (WP-19,
`crates/sandboxd/tests/real-host/`) the stopped time grows by about 3.2 ms per MiB of scratch in use: 129-139 ms near
empty, 5.9 s at 2000 MiB, 15.5-15.8 s at 4800 MiB, so with `maxScratchMib` 512 a moved sandbox stops for up to about
1.7 s more than the memory switch. `maxDowntimeMs` bounds only the memory switch, not the scratch delta.

### 13.11 Packing (pure function, ported as is)

Hosts in use, least loaded first (tie: least recently moved); each host's sandboxes, biggest first, best-fit onto the
other open hosts in use; if all fit, that host is emptied in the plan; moves are returned only if the plan uses
strictly fewer hosts. Size = `ceil(memory / sandbox.memory)` slots (A3.F32). Unit tests ported from `packing.test.ts`.

### 13.12 Release and destroy

`release(row, reason, force)` sets `release_pending = {reason, note, code, force, deadlineAt, askedAt, refusals}` and
runs under the row's `release` claim (taken at once, or, if a start or migration holds the row, by that task when it
ends). Three kinds:

**Soft** (`force: false`: `idle`, `lifetime` before the hard limit, `pool`, `requested`, `recycle` with `onlyIdle`). The
row stays `ready` while the supervisor is asked:
1. `POST http://<ip>:7070/v1/lifecycle/release {reason, code: "sandbox_released", force: false, deadlineMs: 30000}`
   (35 s timeout).
2. 200 `{released: true}` -> `status = 'releasing'`, `bump`, destroy (below), `gone`.
3. 409 `sandbox_busy` -> not released: `release_pending` is cleared (for `requested`, it stays and is retried at the
   next report that says idle), `last_active_at = now()`, event `release-refused {reason, state}`, the claim is
   cleared. The janitor considers the sandbox again only by its rules (idle again for 30 min, or lifetime, whose hard
   limit then forces).
4. No answer (error or timeout) -> one retry after 5 s; if the gateway is answering and the health check (3.10) also
   gets no answer twice, the supervisor is gone: destroy, `gone` with the asked reason and code. If the gateway is not
   answering, nothing is decided (the pending release is retried after the gateway's settle time).
A `PUT` for the conversation while a soft release is pending (not yet answered 200) cancels it (3.6.2); the activity
reply carries the pending release (3.7.3) in case the direct call was lost.

**Forced** (`force: true`: `deleted`, `offboarded`, `not_allowed`, `operator`, `recycle` with `onlyIdle: false`,
`lifetime` at the hard limit):
1. `status = 'releasing'`, `bump`.
2. `POST …/v1/lifecycle/release {reason, code: "sandbox_released", force: true, deadlineMs: 30000}`; the supervisor ends
   a running turn cleanly and answers 200.
3. Destroy after the 200 or at `deadlineAt` (30 s), whichever is first; errors are logged and the destroy goes ahead.

**Lost** (`unhealthy`, `vanished`, `host_lost`, `migration_failed`): no supervisor call; destroy at once; `gone` with
`sandbox_lost` (the feed tells magpie, which fails the running turn with `sandbox.lost`).

**Destroy**: fetch the console tail (`GET …/log?tail=65536`, 5 s, best effort) into `sandbox_logs`; sandboxd `DELETE
/v1/sandboxes/<name>` on its host (or on every reachable host when the host is unknown or gone; also on `migrating_to`
if set). A missing sandbox is fine. Then one transaction: `status='gone', released_at, release_reason, release_note,
release_code, release_message`, clear the claim, `bump`.

A release cut by a replica's death is resumed by the takeover sweep within about 17 s from `release_pending` and the
row's state (a `releasing` row skips straight to destroy once its `deadlineAt` passed); the sandbox stays usable until
`gone`. The janitor's 10-minute `stuck` rule covers only a host that never answers.

### 13.13 Reasons (`ReleaseReason`) and codes

| Reason | Code | Kind | From |
|---|---|---|---|
| `idle` | `sandbox_released` | soft | janitor |
| `lifetime` | `sandbox_released` | soft at 8 h, forced at 9 h | janitor |
| `pool` | `sandbox_released` | soft | starter (pool victim) |
| `requested` | `sandbox_released` | soft | magpie |
| `deleted`, `offboarded` | `sandbox_released` | forced | magpie |
| `not_allowed` | `sandbox_released` | forced | janitor |
| `operator` (note = the operator's note), `recycle` | `sandbox_released` | forced (recycle `onlyIdle`: soft) | admin |
| `unhealthy` | `sandbox_lost` | lost | janitor silence check |
| `vanished` | `sandbox_lost` | lost | scaler (OOM, crash) |
| `host_lost` | `sandbox_lost` | lost | scaler |
| `migration_failed` | `sandbox_lost` | lost | migrate task |
| `pool_full`, `hosts_full`, `no_host_in_time` | `sandbox_busy` | never ready | starter |
| `start_failed` (note = error class), `start_timed_out`, `supervisor_timeout`, `workspace_unavailable` | `sandbox_failed` | never ready | starter, janitor |
| `stuck` | (kept) | | janitor |

The supervisor receives `reason` and `code` on every release it is asked for (3.10); it reports a turn it ends as
`canceled` for `sandbox_released` and as failed for `sandbox_lost`.

### 13.14 Image digests

`sandbox.image` is pinned by digest (`…@sha256:<64 hex>`), so hermit needs no registry lookup: the digest is the
pin. A host that just became active gets `POST /v1/images {digest}` (warm-up). Running sandboxes keep their image; new
ones use the configured one (A3.F34). sandboxd's 4-slot pull table and 20 min wait are unchanged (A5.F39).

### 13.15 sandboxd client

HTTPS to `https://sandboxd:7443` resolved to the host's `wg_ip` (reqwest `resolve`), TLS 1.3, CA `HERMIT_FABRIC_CA`,
client identity `HERMIT_FABRIC_CLIENT_CERT/KEY` (CN `hermit`), keep-alive, at most 8 connections per host (sandboxd
allows 16). JSON; status >= 400 -> `SandboxdError {status, body}`. Calls: `GET /v1/host`, `PUT /v1/peers`,
`PUT /v1/egress`, `POST /v1/images`, `GET|POST /v1/sandboxes`, `DELETE /v1/sandboxes/{n}`, `GET …/spec`, `GET …/log`,
`POST|GET …/receive`, `POST …/send` (A3.F57). Timeouts: heartbeat 5 s, create 15 min, send 15 min, receive wait 95 s,
log 5 s, others 30 s.

---
## 14. Hosts, gateway, AMI and PKI

### 14.1 Host setup and the AMI (moved to `aviary/fabric/host`, `aviary/fabric/aws`)

- `fabric/host/setup.sh hub|burst` is ported with the new paths and names (A5.F47): the gVisor fork release
  (`fabric.runsc`, sha256 checked, `.release`), crane 0.20.6, juicefs 1.4.1 + `sbx-jfs.service` (no writeback, cache 4
  GiB, open-cache 0, `--root-squash 1000:1000` (15.4), background jobs on (no `--no-bgjob`), `Restart=always`, waits for
  a non-kernel `wg0` route, A8.F2, A18.F20), the sandboxd binary from release `fabric.sandboxd`, `wg.key`, `networkd
  ManageForeignRoutes=no`, sysctls, `run-sbx-ckpt.mount` (tmpfs 100 %), `sbx-cgroup`, `sandboxd.service`
  (`KillMode=process`, `Restart=always`, `LimitNOFILE` 1M, `LimitMEMLOCK` infinity, the cgroup leaves and limits of
  A5.F44), logrotate for `/var/log/sbx`. Burst hosts get `sbx-userdata.service` (IMDSv2 user data -> `/etc/sbx/*`).
- `fabric/aws/infra.sh` reads its YYC constants (region, subnet, template, tags, security group name `hermit-host`)
  and the three pins (`sandbox.image`, `fabric.sandboxd`, `fabric.runsc`) from `hermit.json` (passed with `--config`)
  instead of hard-coding them (gap-2 2.21). Commands: `sg`, `ipv6`, `ami`, `template <ami>`, `current`, `check`,
  `sync`, `prune-amis`, `iam`, `iam-key`, `ci-iam`, `ci-iam-key`. AMIs are tagged `image`, `sandboxd`, `runsc`.
- The JuiceFS mount stays on every AWS host for `/workspace` only; nothing on a host reads `users/` or `sent/` any more.
  The gateway runs no sandboxes and mounts no JuiceFS.

### 14.2 The gateway in GitOps (`hermit-gateway` DaemonSet, namespace `yyc-ai-fabric`, new Talos cluster)

Today's gateway is a hand-installed systemd unit no rollout manages (A7.F34, gap-2 2.22). The new one runs on the new
Talos cluster (decision 13), which has no shell, SSH or systemd units, so the gateway is a pod that runs sandboxd
itself; it is installed and upgraded by `yyc-ai.yml` (the deploy workflow), in namespace `yyc-ai-fabric` (PSS
privileged, nothing else in it, platform R24/9.6):
- DaemonSet `hermit-gateway`, `nodeSelector: {hermit.yyc/gateway: "true"}` (one fsn1 Talos node in network `yyc`,
  label set by `fabric/host/gateway-net.sh` through the Kubernetes API), `hostNetwork`, privileged, tolerations all,
  PriorityClass `serving`, `updateStrategy: RollingUpdate, maxUnavailable: 1`, `terminationGracePeriodSeconds: 30`.
- Main container `sandboxd` (image `ghcr.io/de0ch/sandboxd:<sha12>`, which holds the static sandboxd binary and
  crane): sandboxd in gateway mode with `SBX_HOST_JSON` pointing at the mounted `host.json` from ConfigMap `hermit-gateway`
  (role hub, `gateway: true`, `name hermit-gw`, `wgAddress 10.202.0.1`, `hostCidr 10.202.0.0/24`, wg port 51821,
  `controlCidr` (network `yyc`, 10.0.0.0/16), `sandboxCidr 10.203.0.0/16`, `egressVip 10.202.1.1`, `egressPorts
  [[3128,3128]]`, `podCidr` (the new cluster's `<pod-cidr>`), `privateGateway 10.0.0.1`, `reserveMemory 0`, limits;
  sandboxd compiles no address in and refuses to start without `name`, `wgAddress`, `hostCidr`, `controlCidr`,
  `sandboxCidr`, `egressVip`, `privateGateway` and `podCidr`, checking them before it touches anything, so a ConfigMap
  missing one leaves the running data plane as it is (WP-19, 2026-11-02; crates/sandboxd/docs/HOST.md "host.json")),
  the TLS files and `wg.key` from Secret `yyc-ai-fabric/hermit-gateway` (hub server cert/key, CA, `wg.key`), state and
  the image cache (`:7444` image service) on hostPath `/var/lib/hermit-gateway`, memory 256Mi request = limit (the old
  unit's `MemoryMax`). Readiness: a TCP connect to `10.202.0.1:7443`.
- **The data plane is the host's, not the process's.** In gateway mode sandboxd leaves `wg0`, its nft table, its ip
  rules and routes in place when it exits (SIGTERM or a crash) and at start adopts what is there, reconciling it to
  `host.json` and the peer and egress sets saved under `/var/lib/hermit-gateway` (never flush-and-recreate; hermit
  pushes the full sets again within 5 s anyway, 13.8). A pod restart or a DaemonSet update therefore interrupts only
  the control API (:7443) and the image service (:7444) for seconds; hermit's calls retry; sandboxes do not notice
  (A5.F46). There is no `gateway.sh`, no `nsenter` and no host systemd unit.
- Sysctls the gateway needs on its own interfaces (forwarding, `rp_filter` loose on `wg0` and on the private
  interface, where egress pods' replies arrive from pod addresses the node itself routes through Cilium) are set by
  sandboxd at start in the host namespace (a hostNetwork privileged pod may write `net.*`); kernel features (WireGuard, nftables
  with NAT) are built into the Talos kernel (DESIGN 15.2 N7 checks them on the node).
- Prerequisites outside Kubernetes, done once by `fabric/host/gateway-net.sh` (Hetzner API with `YYC_HETZNER_API`, and
  the Kubernetes API; no node shell): the Hetzner network route 10.202.0.0/15 -> `<gw private IP>`, the Hetzner
  firewall rule udp 51821 from anywhere (IPv6) on that server, and the node label. If Talos's ingress firewall is
  enabled on that node, its rules (DESIGN 15.2 N7) are the builder's machine config.
- **Coexistence on Talos** (DESIGN 15.6 "The path on Talos", inputs I2, I3, I6): sandboxd's gateway nft table also
  clamps the MSS of SYNs forwarded from `wg0` to 1280 (`tcp option maxseg size set 1280`: the 1320-byte pod MTU minus
  40; the route's own MTU, 1450, would be too large), so a sandbox's full-size segment never meets the pod MTU
  unclamped; it uses route table 51822 and rule priorities 1000/1001 (after the local table's rule, 100 on
  Talos) and no fwmark, which
  KubeSpan, Cilium and the WARP link `warp0` do not use; the Talos firewall's table and Cilium's rules are other
  tables, which sandboxd never touches and which leave its table alone (N7's kill-and-reapply test). The forward path
  to egress pods is network `yyc` through the cloud controller's per-node pod routes, not KubeSpan or VXLAN. WP-19
  adds the clamp and the private interface's `rp_filter`.

### 14.3 The AMI follows the pins

YYC workflow `hermit-ami.yml` (platform 13.5; replaces `yyc-ai-ami.yml`): on a push touching
`yyc/deploy/hermit/hermit.json` where `sandbox.image`, `fabric.sandboxd` or `fabric.runsc` changed (a `jq` diff
against the parent commit; allowlist, prompt or catalogue edits never bake), checks out **aviary at the commit pinned
in `yyc/deploy/kustomization.yaml`** (not `main`, gap-2 2.21), runs `fabric/aws/infra.sh --config hermit.json sync`,
then `check` must exit 0 (A19.F22, A20.F24, A7.F50). Hosts launched before the new AMI exists fetch the image from the
gateway (:7444, about 45 s more).

### 14.4 AWS identities

- `hermit-scaler` (IAM user, keys in store `yyc` as `YYC_HERMIT_AWS_ACCESS_KEY_ID/SECRET`): `ec2:RunInstances` only with
  launch template `hermit-host`, `ec2:CreateTags` on create, `ec2:TerminateInstances` only on instances tagged
  `app=hermit-host`, `ec2:DescribeInstances`. Made by `infra.sh iam` / `iam-key` (gap-2 2.19).
- `hermit-ami-ci` (IAM user for the AMI workflow, YYC GitHub secrets `YYC_HERMIT_AMI_AWS_*`), `infra.sh ci-iam`.
- eu-central-1 vCPU quota 16 = 4 m7i.xlarge across both fabrics during the switch window (22.2); `maxHosts` 3.

### 14.5 PKI (`aviary/fabric/host/make-certs.sh`)

A new EC P-256 CA (10 years, CN "hermit fabric CA") and leaves (3 years): the gateway's and the shared AWS hosts'
server certificates (CN and SAN `sandboxd`), and hermit's client certificate (CN `hermit`, clientAuth) (A5.F48). Stored
in `yyc` as `YYC_HERMIT_FABRIC_CA`, `_CA_KEY` (store only), `_CLIENT_CERT/_KEY`, `_HOST_CERT/_KEY`, `_GATEWAY_CERT/_KEY`.
Rotation runbook (new, gap-2 2.18): (1) make a new CA and leaves; (2) sandboxd and hermit accept a CA bundle (both CAs);
roll hermit and the gateway DaemonSet; bake an AMI whose user data carries the bundle; (3) switch hermit's client cert
and the host certs to the new leaves; (4) once no host from before step 3 lives (at most 9 h of sandbox life plus the
900 s host minimum), drop the old CA from the bundles.

### 14.6 sandboxd releases

aviary's `ci.yml` (platform 6.1) runs sandboxd's fmt, clippy, unit tests, static musl build, `sandboxd budget` and the
45 s soak (two daemons with fake runsc/ip/nft/wg, hostile clients, RSS flat under the budget) (A7.F47, A20.F13); on
`main` the release `aviary-<sha12>` carries the binary with `SHA256SUMS`, and image `ghcr.io/de0ch/sandboxd:<sha12>`
(for the gateway DaemonSet) is pushed.

### 14.7 The single gateway node: the stated gap and its maintenance runbook

The gateway (WireGuard hub, the egress VIP's DNAT, the route into the overlay) runs on one Talos node. While that node is
down, every sandbox has lost its egress and control path: running turns stall on model calls and FUSE writes (the
supervisor retries; nothing is lost), new starts wait and end `sandbox_busy` after 170 s. **A reboot or replacement of
that node is therefore not zero-downtime for running turns.** This is stated, not hidden: the fix (two gateway nodes,
both peered from every AWS host, the VIP and the Hetzner network route failing over) needs a route swap through the
Hetzner API on failure and a second WireGuard endpoint per host; it is a separate change, not part of this refactor.

What keeps the gap small:
- The gateway node is excluded from routine node work. The new cluster's weekly Talos upgrade workflow (the Talos
  session's) upgrades a node labelled `hermit.yyc/gateway=true` only by calling this runbook (`hermit-admin.sh
  gateway-maintenance --node-work "<the workflow's upgrade of that node>"`), and holds every node while
  `yyc-ai/cutover/window.json` says `"nodeWork": "hold"` (DESIGN 15.2 N8); the cluster README says so. Hetzner
  maintenance notices for that server are handled with the same runbook at a chosen time.
- While the gateway is unreachable hermit decides nothing about sandboxes: AWS hosts are not judged lost (13.4 step 4),
  the silence rule pauses until the gateway has answered for 120 s (12 step 3), pending soft releases wait (13.12). Idle
  sandboxes and their held background work survive a reboot of a minute or two.
- A restart of the gateway pod (a sandboxd upgrade, a DaemonSet update) leaves WireGuard and nft untouched (14.2), so
  upgrades of the gateway itself are not outages; only the node's own reboot is.

Runbook "gateway node maintenance" (`aviary/fabric/host/gateway-maintenance.md`, scripted by
`hermit-admin.sh gateway-maintenance`):
1. `PUT /v1/admin/sandboxes-enabled {"enabled": false, "reason": "gateway maintenance"}`: new starts answer
   `sandbox_busy` (`disabled`) at once; running sandboxes are untouched.
2. Wait until the ops snapshot shows no busy sandbox (`sandboxes.list[].busy` all false), or a chosen deadline.
3. Do the node work (the Talos upgrade or reboot of that node, run by the Talos session's workflow). The DaemonSet pod returns with the node; its
   install step is idempotent.
4. Wait for `hermit-gw` heartbeats again (ops: gateway answering) and the egress targets pushed.
5. `PUT /v1/admin/sandboxes-enabled {"enabled": true}`.

---

## 15. Workspaces (JuiceFS)

### 15.1 The volume (kept as it is)

hermit uses the existing JuiceFS volume `yyc-ai` (UUID b3bb55d3-7da1-47af-bd5f-4ead598274ce) and changes nothing in its
format (A18.F0): storage s3, bucket `yyc-ai-files` with the data under the prefix `yyc-ai/` (Hetzner fsn1), block size
4 MiB, no compression, **TrashDays 30** (Deyao, 2026-11-16: a deleted file stays in the volume's trash for 30 days,
YYC Drive's safety net; a purged person's files leave the bucket 30 days after the purge), **no data encryption** (no EncryptKey; the S3
secret is encrypted in the metadata), DirStats on (the directory quotas need it), metadata in database `jfs_yyc_ai`
(role `juicefs`) on cluster `db/pg`. The CSI mount pods run `juicefs format --no-update`, which never rewrites the
format. The layout stays `ws/<id>` for every conversation (unchanged ids), so no workspace moves at the switch.

### 15.2 Clients after the switch

| Client | Where | Mount options | For |
|---|---|---|---|
| hermit's CSI mount pods | one per node that runs a hermit replica (two), created by the JuiceFS CSI node plugin for static PV `hermit-jfs` (`volumeHandle yyc-ai-hermit`, PVC `yyc-ai/hermit-jfs`, Secret `yyc-ai/hermit-juicefs` with `name`, `metaurl`, `envs: {META_PASSWORD}`) | `cache-size=0`, `open-cache=0`, `all-squash=1000:1000` | hermit's `/jfs` (ensure, list, read, clone, delete) |
| AWS hosts' `sbx-jfs` | every AWS host (14.1) | no writeback, cache 4 GiB, `open-cache=0`, `root-squash=1000:1000` | the sandboxes' `/workspace` binds |
| hermit's `juicefs` CLI | inside hermit's container | none (talks to the metadata directly) | `juicefs quota` |

- The JuiceFS CSI driver HelmRelease stays as it is today (chart 0.33.0 in `kube-system`, `mountMode mountpod`, mount
  image `juicedata/mount:ce-v1.4.1`, mount pods 50m/192Mi, node plugin on the cx33/cpx32 nodes; moved unchanged to
  `yyc/deploy/juicefs-csi.yaml`, platform 9.1; A18.F16). hermit's Pods therefore have node affinity to those nodes
  (platform 9.6). The old static PV `yyc-ai-jfs` and its mount pods for the `ui` pods go with `ui` at the teardown
  (A18.F17, A18.F18: same mechanism, now for `hermit-jfs`).
- Mount pods are created at a node's first hermit Pod and removed when the last one leaves (the CSI driver's own
  behaviour, unchanged).

### 15.3 Client background jobs and the metadata backup

- Every mount client runs JuiceFS's background jobs; **no mount uses `--no-bgjob`** (A18.F13): session heartbeats
  (12 s), cleanup of expired sessions with their sustained inodes and locks, slice compaction (retried after a failed
  S3 call).
- The hourly **metadata dump** (`BackupMeta` 1 h, `BackupSkipTrash false`; the shared last-backup time means one client
  per hour takes it) keeps writing `s3://…/yyc-ai-files/yyc-ai/meta/dump-<time>.json.gz` (A18.F11). It protects the
  metadata of `/workspace` (beside the daily locked dump and PITR of `jfs_yyc_ai`, platform 11.2); it has nothing to do
  with magpie's data. After the switch hermit's two mount pods are always running, so the dump continues when no AWS
  host exists; the AWS hosts' `sbx-jfs` may take it too. JuiceFS prunes old dumps itself; the dump's harmless warning
  about the trash root inode is unchanged.

### 15.4 Ownership: everything under `ws/` is uid/gid 1000

- **hermit** acts as uid/gid 1000 on the volume (`all-squash=1000:1000`), whatever uid its process has (65532): every
  folder it makes (`mkdir ws/<id>`, 0755) and every clone is owned by 1000, the sandbox user.
- **Sandboxes** write as uid 1000; writes made as root inside a sandbox (with sudo) are mapped to 1000 by the host
  mount's `root-squash=1000:1000`, so no root-owned entry appears in a workspace any more. (A `chown` to another user
  inside `/workspace` then fails with EPERM; `tar --same-owner` and similar warn. The sandbox rules already say
  `/workspace` is the user's area; the acceptance test of 24.3 checks pip, npm, apt-cached files and `git clone` as
  root still work.)
- **Existing entries**: today `ws/` itself is root-owned and the old fabric's sandboxes may have left root-owned files.
  A one-time Job `hermit-jfs-owner` (namespace `yyc-ai-fabric`, image `hermit`, root, privileged for FUSE) mounts the
  volume without squashing and runs `chown 1000:1000 /jfs/ws && chown -R 1000:1000 /jfs/ws/*`; it runs in cutover P2
  before the first import pass, and again after the old fabric is gone (P5), since the old sandboxes write as root
  until then. It replaces sandboxd's chown of `/ws` and `/users` on every create (A18.F5) and today's EACCES leftovers
  (A17.F4).
- **Who deletes `ws/<id>`, and as whom**: hermit's `DELETE /v1/workspaces/{wid}`, through its own mount, as uid 1000
  (`juicefs rmr` needs write and search permission on every directory of the tree, which the ownership rule gives). A
  delete that still meets EACCES (an entry the fix-up missed) answers 503 `workspace_unavailable`, logs `workspace
  delete refused` with the path, and the ops verdict shows a problem; magpie retries from its outbox; the operator
  re-runs `hermit-jfs-owner`.

### 15.5 Operations

| Operation | How |
|---|---|
| ensure (`PUT /v1/workspaces/{wid}`, and step 2 of every start) | `mkdir /jfs/ws/<wid>` (0755) if absent; then `juicefs quota get` for `/ws/<wid>`; when absent or different, `juicefs quota set $META --path /ws/<wid> --capacity <quotaGiB> --inodes <quotaInodes>` (60 s timeout). Idempotent and always run, so every workspace has its quota whatever made the folder (A18.F2, A18.F3) |
| usage (`GET`) | `juicefs quota get` (used bytes and inodes), cached 10 s |
| list, read | `openat2` beneath `/jfs/ws/<wid>` (3.8 rules); reads stream with `Range`; at most 64 concurrent reads per replica |
| clone | flush a live sandbox (3.8); ensure the target does not exist; `juicefs clone /jfs/ws/<from> /jfs/ws/<wid>` (metadata copy-on-write, seconds for 1 000 files); then ensure (quota) |
| delete | release the live sandbox if any (3.8); `juicefs rmr /jfs/ws/<wid>` (server-side, fast); `juicefs quota delete --path /ws/<wid>` |

The 10 empty folders found live are removed by `legacy-cleanup` (22.3 step 6).

### 15.6 Failure, and a mount-pod restart under a running hermit Pod

- hermit's `/jfs` volumeMount uses `mountPropagation: HostToContainer` (allowed under the restricted Pod Security
  level), so if a mount pod is re-created after a failed FUSE-fd hand-over, the new mount appears inside the running
  hermit container; a smooth restart (the fd passed over `JFS_SUPER_COMM`) is invisible anyway (A18.F19).
- While `/jfs` is broken in one replica (stat of `/jfs/ws` fails with `ENOTCONN`/`EIO`): that replica's workspace calls
  answer 503 `workspace_unavailable` with `retry-after: 2` (callers retry and usually reach the other replica), and a
  start at its workspace step is handed to another replica (11.2 step 2). Its **readiness does not change**: readiness
  never depends on JuiceFS, because a JuiceFS blip must not take people lookups, deployment facts, the feed and token
  liveness out of the Service on every replica at once. The ops verdict shows `problem: workspace store not mounted in
  hermit <pod>`; a mount still broken after 5 min is fixed by deleting that Pod (the Deployment replaces it; runbook in
  the yyc-ai skill).
- The volume's metadata database down: workspace calls 503 everywhere; starts fail at step 2 after 3 tries with
  `sandbox_failed`; running sandboxes' JuiceFS clients wait and retry (JuiceFS behaviour).

---

## 16. Admin routes (YYC; replace every SQL-only knob)

All need the admin token and `Aviary-Admin`; each writes an `admin_events` row in the same transaction as the change.
Called by the operator with `kubectl -n yyc-ai port-forward svc/hermit 18080:80` or from a runbook script
(`aviary/crates/hermit/scripts/hermit-admin.sh <verb> …`, which takes the token from the store).

| Route | Body | Effect |
|---|---|---|
| `GET /v1/admin/hosts` | | live hosts with slots, sandboxes, `retire_by`, cap |
| `PATCH /v1/admin/hosts/{name}` | `{"maxSandboxes": int\|null, "retire": true \| "reactivate": true, "note": "…"}` | cap a host (0 = no new sandboxes; null = by memory/CPU), retire it (`retire_by='operator'`: its sandboxes move off, then it is terminated), or set a retiring host active again (A3.F35, gap-2 2.7, 2.23) |
| `PUT /v1/admin/sandboxes-enabled` | `{"enabled": false, "reason": "…"}` | while off, `PUT /v1/conversations/{id}/sandbox` answers 503 `sandbox_busy {reason: "disabled"}` at once and the scaler launches nothing; running sandboxes are untouched (gap-2 2.9; the gateway runbook, 14.7) |
| `POST /v1/admin/fresh-host` | `{"ttlMinutes": 1..60}` | until the TTL passes, only hosts that become active after now are placeable, so the next start launches a host (the "real wait" test, gap-2 2.8, A20.F24); it ends by itself, so a test cannot leave a cap behind |
| `GET /v1/admin/sandboxes?conversation=&person=&since=` | | sandboxes with their starts, phases, releases and migrations (the trace tool, platform 15.2) |
| `GET /v1/admin/sandboxes/{id}/log` | | the stored console tail (platform 15.4) |
| `POST /v1/admin/sandboxes/{id}/release` | `{"note": "…"}` | forced release `operator` with that note |
| `POST /v1/admin/sandboxes/{id}/exec` | supervisor 15.4's `/v1/exec` body | forwards to that sandbox's supervisor (isolation probes and debugging, gap-2 2.13); `admin_events` records the argv and exit code, never stdin or output |
| `POST /v1/admin/recycle` | `{"exceptImage": "sha256:…", "onlyIdle": true}` | releases every live sandbox on another image (soft with `onlyIdle`, forced without), e.g. after a supervisor protocol change (A15.F17) |
| `POST /v1/admin/people/{email}/purge` | `{"confirm": "<the same email>"}` | the only deletion of a person's data: 409 `still_allowed` while the catalogue allows the person; otherwise forced-releases their live sandboxes (`offboarded`) and calls magpie `POST /v1/hermit/persons/{person}/purge {"reason": "offboarded"}` (magpie 9.6), answering magpie's 202 `{conversations: n}`; idempotent |
| `GET /v1/admin/events?limit=` | | the audit log |

Removing a person from the catalogue blocks them within about 2 min (3.4.1) and releases their sandboxes (12 step 4);
it deletes nothing. Whether and when a leaver's conversations are deleted is the operator's decision through the purge
route (Deyao, decision 7, 2026-11-02: manual operator purge only, never automatic). The ops page shows operator intent apart from the system's
(`retire_by`, caps, the disabled switch, fresh-host until).

---

## 17. The sandbox image (`aviary/images/sandbox`, `ghcr.io/de0ch/hermit-sandbox:<sha12>`)

Ported from jarvis3 `chat/sandbox/Dockerfile` with the agent replaced by the supervisor (supervisor 3):
- Base `node:22-trixie-slim`; apt bash, curl, git, jq, unzip, zip, file, procps, sudo, python3 + venv, poppler-utils,
  pandoc, fonts-noto-cjk/core, pango/harfbuzz-subset, `fuse3` (for `fusermount3` if the supervisor needs it); venv
  `/opt/py` (pandas, openpyxl, xlsxwriter, python-docx, python-pptx, matplotlib, pypdf, pdfplumber, bs4, lxml,
  requests, httpx, Pillow, markdown, tabulate, dateutil, chardet, opencc, weasyprint, pymupdf<2, reportlab); the
  WeasyPrint CJK round-trip check; the TrueType Noto Sans/Serif HK built at a pinned commit and sha256-checked (A5.F0).
  No tini.
- `reportlab-cjk.py` `.pth` finder (A5.F1); docker-ce, containerd, buildx and the `docker` wrapper, which asks the
  supervisor to start dockerd with `GET /start` on `/run/supervisor/docker.sock` (supervisor 4.6; A5.F2); the apt
  wrapper, `90sandbox`, `sudoers.d/sandbox` (A5.F4); uid 1000 `node` with home `/home/sandbox`, `/home/sandbox/.claude`
  (an empty directory: the FUSE mountpoint), `/work`, `/workspace`, PATH, env defaults and
  `/etc/profile.d/00-sandbox-path.sh` (A5.F5); `/opt/image-setup/` with the Dockerfile and the files it installs
  (A5.F6; the instance prompt still points there).
- Claude Code at the version the supervisor spec pins (2.1.304 today), installed root-owned at `/opt/claude-code`
  (supervisor 3).
- `/usr/local/bin/supervisor`, `/usr/local/bin/supervisor-keeper`, `/usr/local/bin/sandbox-secret -> supervisor`;
  `ENTRYPOINT ["/usr/local/bin/supervisor-keeper"]` (PID 1, platform R12), no `CMD`. No Node agent, no `/opt/agent`,
  no `PORT=7070` env, no `/run/sbx-docker`.
- CI smoke (aviary `ci.yml`, A20.F12): image sizes; `/opt/claude-code/bin/claude --version`; `python3 -c 'import
  pandas'`; uid 1000; `/workspace` exists; dockerd/containerd present; `docker ps` says not available without the
  supervisor's socket; sudo keeps `HTTPS_PROXY`; `apt-get` is the wrapper; uid 1000 can `apt-get install sl`; without
  root (no-new-privileges) apt-get exits 100; `supervisor --version`; the keeper started with no proxy credential in
  its environment exits non-zero with "no sandbox proxy credential" (never a crash loop that hides the cause); Claude
  Code's API and MCP requests reach a test proxy in absolute form (supervisor 17.2).

## 18. Operator page, snapshot, budget display and RBAC

### 18.1 Access

`GET|HEAD /ops/` and `/ops/snapshot?tz=` (3.11). pigeon forwards an admin's browser (`Aviary-Person`, re-checked
against the catalogue on every request); the news-stream monitor uses its pair token (`HERMIT_ACCEPT_MONITOR`;
read-only scope: the snapshot only, no person, no view-as); the admin token may read both. Gone: the shared
`JARVIS_CC_TOKEN` and the `x-jarvis-user` header (A3.F6, A19.F4, A19.F6).

### 18.2 The page (`/ops/`)

`jarvis3 web/ops-page.ts` ported as a static HTML file compiled into hermit (`include_str!`), with the same strict CSP
(`default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:;
frame-ancestors 'none'`), `no-store`, `nosniff`, `no-referrer` (A3.F37). Changes: the title is `<product.name> Ops`
(was hard-coded "YYC AI Ops"); the default zone is `ops.timeZone`; node names lose `ops.podNodePrefix`; English
throughout as today. Behaviour kept: polls `./snapshot?tz=<browser zone>` every 10 s while visible, "not updating" after
30 s without data; Now/History tabs on phones (`#now`/`#history`), side by side from 1 100 px; no message contents
anywhere.

Now: the verdict pill and findings; Deployments and pods per `ops.deployments` (pigeon, magpie, hermit, egress, tools,
cloudflared); the gateway; AWS hosts (sandboxes/slots bar, memory, load, uptime, cap and who set it, empty since,
instance, gVisor); live sandboxes (person, host, state and phase, busy, age, idle, migrations, a pending or refused
release, "not reporting"); the scaler plan and the warm hints; the operator switches (sandboxes disabled, fresh-host
until); the budget bar; errors of the last 15 min. History: starts and releases with p50/p95, host events (60), hosts
gone (24 h), egress errors (24 h), spend per day, person and model, conversations per staff member each with a 「View
as」 form posting to `/api/jarvis/view-as` (pigeon, same origin, new tab; not for former users), the view-as log.

### 18.3 The snapshot (`/ops/snapshot?tz=`) and the monitor's two contracts

- `tz` validated as an IANA name (`chrono-tz`), else `ops.timeZone`; "today" and "this month" start at that zone's
  midnight. Cached 4 s per zone per replica (A3.F38, A19.F4).
- 13 sections built in parallel, each with an 8 s timeout; a failed section is `null` and `errors[<name>]` holds the
  message (at most 300 characters). Calls to magpie and egress have a 5 s timeout.
- **Contract 1, `monitor.py ai_slim` (A19.F1): every field it reads keeps its name, type and unit** (`at`, `today`,
  `health{state, findings[{level, about, text}]}`, `errors`, `service.deployments[{name, ready, desired, state,
  pods[{restarts}]}]`, `hosts{maxHosts, list[{name, kind, gateway, status, reachable, sandboxes, slots, cpus, load1,
  memTotal, memAvailable, launchedAt, instanceId}]}` (bytes), `sandboxes{maxActive, list[{owner, status, host, ageS,
  idleS, migrations}]}`, `budget{status{spentUsd, limitUsd, since, exhausted}, total{today, month, callsToday},
  byUser[{owner, today, callsToday}], byModel[{model, today, callsToday}]}`, `people[{owner, today, live}]`,
  `recent{failedStarts, modelErrors, hostErrors, blocked}`, `recentMinutes` 15, `starts.summary{n, ok, failed, p50Ms,
  p95Ms}`, `scaler.plan{available, demand, waiting, planned, pending}`). `kind` for the gateway is now `"gateway"` (was
  `"hub"`); `ai_slim` only reads `gateway: true`.
- **Contract 2, `monitor.py ai_activity` (A19.F2): the event-diff fields keep their names, values and format.** It
  diffs these against its cursor `live:ai:cursor` (kept in the news stream's own database) by lexical comparison of
  `at`, so `at` must stay RFC 3339 UTC with exactly three fraction digits and `Z` everywhere below:
  - `starts.list[{at, what, owner, host, ok, ms, reason}]`: `what` is `"start"` (a `sandbox_starts` row) or
    `"release"` (a sandbox's `released_at`, `ms` its lifetime) and nothing else; `owner` is the person's **email**
    (never `person_key`); `reason` is the release reason (13.13) or a failed start's reason, verbatim (the monitor shows
    it, it does not parse it); the 80 newest of the last 24 h;
  - `events[{at, kind, host, owner, data}]` (the 60 newest `host_events`, `data.error` when there is one);
  - `egress.recent[{at, kind, host, path, status, decision, owner}]` (egress's, passed through);
  - `budget.byUser[{owner, callsToday, today}]`, `budget.byModel[{model, callsToday}]`, `today`, `health.state`,
    `health.findings[].text`.
- **What `ai_activity` sees at the switch**: the monitor's cursor is its own and carries over unchanged. Imported rows
  keep their original `at` (sandbox starts, releases and host events of the old fabric, 22.3), all older than the
  cursor, so they produce no duplicate cards. Rows of the old fabric that the delta import brings in after the monitor
  has already seen newer hermit events are not shown as cards (they are in the history lists): missed, never repeated.
  The per-day call counts come from egress, whose import keeps its rows (platform 17.2), so the first diff after the
  switch shows no spurious burst. The URL and token change in the switch commit (21.4).
- Both contracts are guarded by a golden test in hermit's integration suite (24.2: the field paths, types and the
  value rules above, `at` format included, against a fixture built from live-shaped rows) and by harness scenario E16
  (platform 7.8).

| Section | Source | Shape (beyond the contract fields above) |
|---|---|---|
| top level | hermit | `at, tz, today, monthStart, product, fabric: "gvisor-aws", sandboxImage, recentMinutes, controls{sandboxesEnabled, disabledReason, freshHostUntil}, catalogue{generation, loadedAt, refused: string\|null}` |
| `service` | k8s API (18.5) | `{available, deployments: [{name, missing?, desired, ready, updated, total, rolling, state, pods: [{name, ready, phase, restarts, ageS, node}]}]}` (A3.F40) |
| `hosts` | `hosts` | `{maxHosts, list: [{…, retiring, retireBy, emptySince, maxSandboxes, gvisor, lastSeenAt}]}` |
| `gatewayMissing` | `hosts` | true when no `gateway` row is live |
| `sandboxes` | `sandboxes` live + `host_events` | `{maxActive, idleMinutes, maxLifetimeHours, list: [{id, conversationId, owner, host, status, phase, busy, createdAt, lastActiveAt, expiresAt, ageS, idleS, migrations, releasePending, reporting}]}` (A3.F41) |
| `scaler` | `demand_hints`, `plan()` | `{windowS: 120, hints: [{who, at, counted}], plan: {need, demand, waiting, planned, available, pending, perHost}}` |
| `recent` | `sandbox_starts`, `host_events` + egress | `{failedStarts, hostErrors, modelErrors, blocked, list: [{at, what, detail, owner}]}` (20 newest; A3.F43) |
| `budget` | egress | as egress returns it (18.4) |
| `hostsGone` | `hosts` | `{count24h, list: [{name, kind, instanceId, launchedAt, goneAt, goneReason, livedS}]}` (30 newest) |
| `events` | `host_events` + `sandboxes` | 60 newest `{at, kind, host, sandboxId, owner, conversationId, data}` (A3.F49) |
| `starts` | `sandbox_starts` + `sandboxes` | `{summary, releases: [{reason, n}], list: [{at, what, ok, ms, host, conversationId, owner, reason}]}` (80, A3.F42) |
| `egress` | egress | `{byStatus, recent, deniedHosts}` (A6.F23) |
| `people` | catalogue + magpie + `sandboxes` | `[{owner, total, today, last7d, live, deleted, purged, lastActiveAt, former?}]` (A17.F16) |
| `viewAs` | magpie | `[{at, conversationId, event, admin, user, session, chars}]` (100 newest, A3.F47) |

`people`: every explicit catalogue user is listed (zeros when magpie has nothing), plus every person magpie reports
(domain staff appear once they have a conversation). `total`, `today`, `last7d`, `deleted`, `purged`, `lastActiveAt`
are magpie's conversation counts, no longer sandbox rows (1.2). `live` is **hermit's** count of that person's live
sandboxes (the meaning `ai_slim` has always had); magpie's own `live` (conversations with a turn in progress) is not
used. `former: true` for a person the catalogue no longer allows (no View-as button).

### 18.4 What hermit reads from magpie and egress (consumed contracts; shapes exactly as platform 15.3)

```
GET http://magpie/v1/ops/people?tz=<IANA>          bearer HERMIT_TOKEN_MAGPIE
200 {"tz": "Asia/Hong_Kong",
     "people": [{"owner": "<email>", "total": 12, "today": 1, "last7d": 4, "live": 0, "deleted": 2,
                 "purged": 0, "lastActiveAt": "<RFC 3339>" | null}]}

GET http://magpie/v1/ops/view-as?limit=100
200 {"viewAs": [{"at", "conversationId", "event": "start" | "send" | "create", "admin", "user", "session", "chars"}]}

GET http://egress/v1/ops/summary?tz=<IANA>&recentMinutes=15     bearer HERMIT_TOKEN_EGRESS
200 {"budget": {"status": {"spentUsd", "limitUsd", "since", "exhausted"},
                "total": {"today", "month", "sinceBudget", "callsToday", "callsMonth"},
                "byUser": [{"owner", "today", "month", "sinceBudget", "callsToday", "callsMonth"}],
                "byModel": [{"model", "today", "month", "sinceBudget", "callsToday", "callsMonth"}],
                "byDay": [{"day": "YYYY-MM-DD", "usd", "calls", "people"}]},
     "egress": {"byStatus": [{"kind", "status", "decision", "n", "lastAt"}],
                "recent": [{"at", "kind", "host", "path", "status", "decision", "owner", "conversationId"}],
                "deniedHosts": [{"host", "why", "n", "lastAt"}]},
     "recent": {"modelErrors": 0, "blocked": 0, "list": [{"at", "what", "detail", "owner"}]}}
```
- Mapping onto the monitor contract: `people[].owner/today` from magpie, `people[].live` from hermit (18.3);
  `budget.*` and `egress.*` from egress unchanged (`owner` = email, `at` in the millisecond format); `chars` is the
  view-as message length (an integer, never the text).
- egress records the person on its own rows (from the verified sandbox token, 6.3) so `owner` needs no join with
  hermit's tables; the 189 historic calls that only the legacy `conversations.owner` can attribute are backfilled once
  by egress (A13.F5, 22.4).
- hermit also uses magpie's `POST /v1/hermit/workspaces/exists` (legacy cleanup, 22.3) and `POST
  /v1/hermit/persons/{person}/purge` (16).

### 18.5 Kubernetes access

ServiceAccount `hermit` with Role `hermit-read` in namespace `yyc-ai`: `apps/deployments` get/list, `pods` get/list,
`discovery.k8s.io/endpointslices` get/list/watch. Nothing cluster-wide, no writes (A3.F59, platform 9.11). The k8s
reader lists Deployments by name from `ops.deployments` and pods with `app in (…)` (A3.F40: the role list is
configuration of the YYC implementation, not code).

### 18.6 Health verdict (`healthOf`, ported, A3.F39)

Levels `ok < warming < watch < blind < problem`; state = the worst finding. The levels are the flight deck's colours
(Deyao, 2026-11-17): red is only a service lost right now ("immediate loss of service"), for everyone or for one person.
- problem (red): a Deployment missing or with no pod ready; the gateway not answering; no gateway registered; a host
  with sandboxes on it not answering; a sandbox starting > 180 s; the budget exhausted; no egress targets; the
  workspace store unmounted, or hermit silent, in every hermit replica.
- blind (magenta): a section unreadable (what it covers may be fine or down).
- watch (amber), still serving: a Deployment with `0 < ready < desired` while not rolling (its state `watch`); a host
  booting > 300 s; an empty host not answering; the scaler leader's last pass older than 30 s; the catalogue refused
  (the last good one is used); a hermit replica silent or without its workspace store beside a healthy one; a
  workspace delete refused (15.4); budget used >= `ops.budgetWatch` (80 %); sandboxes disabled by the operator; a
  fresh-host test running; a ready sandbox not reporting (silence kept by a healthy answer). And errors that are over:
  failed starts, model/route errors or host errors in the last 15 min.
- warming (blue): a Deployment rolling; a host booting or retiring; a sandbox starting.
- Noise excluded as today: `inject` + 405 (MCP optional GET).

### 18.7 Budget display

hermit shows egress's numbers and never holds a limit: the bar `spentUsd / limitUsd` since `since`, today/month totals,
per person/model/day (A3.F38, A6.F23, A19.F1). A reset or raise is egress's configuration (gap-2 2.2).

---

## 19. Failure modes

| Failure | What happens | Staff see |
|---|---|---|
| One hermit replica dies | its tasks (starts, migrations, releases) are taken over within 17 s (10.3); held long-polls fail and magpie retries on another replica; leadership moves within 5 s (clean) or about 2 min (crash, TCP keepalive) | nothing (a slower start at worst) |
| All hermit replicas down | pigeon keeps signed-in people for 5 min (stale cache) then 503; magpie cannot start sandboxes (it retries until its 15 min deadline); **running sandboxes keep working completely**: the gateway verifies tokens locally and serves its last liveness answers (6.3), so model calls, tool calls and FUSE writes to magpie continue; activity reports fail and are retried; nothing is idled or released while hermit is down | running turns unaffected; new turns wait, then fail `sandbox.timeout` if hermit stays down |
| Postgres down | readiness stays up (20.1); `/v1/deployment`, `/v1/people/*`, token keys and sandbox liveness keep answering from memory; everything else 503 `unavailable`; leaders lose their locks and stop | as above for new turns; sign-in and running turns unaffected |
| Gateway sandboxd down (restart) | :7443 down for seconds; hermit retries; WireGuard and nft keep flowing | nothing |
| Gateway node down | every sandbox loses its egress and control path; hermit judges nothing (no host loss, no silence releases) until the gateway is back and settled; the verdict says problem; maintenance follows the runbook (14.7) | running turns stall until it returns; new starts wait, then `sandbox_busy` |
| AWS host lost | 13.7: its sandboxes `gone/sandbox_lost` within 35 s (30 s silence + one pass); magpie ends their turns | the turn ends with the "stopped during the reply" message; the next send gets a new sandbox |
| Sandbox OOM (sentry killed) | `alive:false` within 10 s -> `vanished`, `sandbox_lost` | same |
| Supervisor crash inside a sandbox | its keeper restarts it within 5 s; health says `ready: false` meanwhile (alive); nothing is released | a short pause (supervisor 19) |
| Supervisor refuses an idle release (it is busy) | the sandbox stays `ready`; `release-refused` event; the idle clock restarts | nothing |
| A replica dies mid-migration | the takeover asks the target and adopts or settles the sandbox (13.10); never `vanished` | nothing |
| sandboxd crash on a host | systemd restarts it; sandboxes keep running (`KillMode=process`); state files recover them | nothing |
| JuiceFS down on a host | no placement there; after 120 s the host retires and its sandboxes move off (the target binds its own mount) | file tools may fail briefly |
| JuiceFS broken in one hermit replica | that replica answers workspace calls 503 and hands starts over; verdict watch beside a healthy replica, problem with none (15.6) | nothing |
| JuiceFS metadata down | workspace calls 503; starts fail at step 2 | new turns fail with `sandbox_failed` |
| EC2 API failing / quota | `launch-failed` events; starts needing a host end `sandbox_busy` after 170 s | 「所有電腦正在使用中…」 |
| Pool full (10 live, none idle) | waits 60 s for capacity, then `sandbox_busy` | same, instead of today's wrong `sandboxFailed` |
| Catalogue file broken | last good list kept; verdict watch | nothing |
| magpie or egress down (ops only) | those snapshot sections are null with an error; verdict blind | ops page partly empty |
| EndpointSlice watch failing | the last egress targets stay on the gateway; verdict problem after 60 s | nothing unless egress pods changed |
| Live migration fails | source keeps the sandbox (`kept:true`), the target's copy is adopted, or it is lost (`kept:false` -> `sandbox_lost`) | nothing, or the lost-turn message |
| A start crosses its deadline | `gone/sandbox_failed`, destroyed | 「未能啟動這個對話的電腦…」 |
| Token key list misconfigured (a live row's kid missing) | hermit refuses to start (checked at start-up), so the bad roll never becomes ready | nothing |

---

## 20. Zero downtime

### 20.1 Probes (port 9090, platform 3.2)

- `/healthz` (liveness): the process answers and the event loop responded within 1 s. Period 10 s, failureThreshold 6.
- `/readyz` (readiness): not draining, migrations applied, `hermit.json` loaded, a catalogue loaded at least once, the
  live map loaded once. Period 3 s. **Not** Postgres, JuiceFS or the k8s API afterwards: a shared-dependency blip must
  not take every replica out of the Service at once (pigeon's people lookups and the gateway's liveness lookups are
  served from memory).

### 20.2 Drain on SIGTERM

1. preStop `sleep 10` (the endpoint leaves the Service first).
2. SIGTERM: `draining = true` (`/readyz` 503, warm answers `hinted:false`, new `PUT`s still answered: they insert the row
   and leave the start to the takeover sweep of the other replicas).
3. Hand back every row claim this replica holds (`task_until = now()`: starts, migrations, releases are resumed by
   another replica within 2 s; sandboxd keeps sending a migration on its own, and the new owner settles it, 13.10).
   Answer held long-polls with their current state at once (callers re-poll elsewhere).
4. Release leadership: unlock 7300/3 and 7300/4 (the other replica leads within 5 s).
5. Wait for in-flight requests (at most 30 s: workspace streams; the biggest is a 1 GiB download, cut after 30 s and
   resumed by the caller with `Range`) and for the supervisor and sandboxd calls already sent (at most 35 s; their
   results are written only if this replica still holds the claim, otherwise the new owner repeats the idempotent
   step).
6. Close the pool, exit 0 (log `drained`). `terminationGracePeriodSeconds: 90`.

Unlike today (A3.F8: up to 300 s for a start to finish in place, and a start outliving the grace left a stuck row), no
work depends on the replica that began it.

### 20.3 Rollout

Deployment `hermit`: replicas 2, `RollingUpdate maxSurge 1 / maxUnavailable 0`, `minReadySeconds 5`, PDB `minAvailable
1`, `topologySpreadConstraints` (hostname, `maxSkew 1`, `matchLabelKeys: [pod-template-hash]`, `DoNotSchedule`;
platform 9.3; today both web pods could share a node, A7.F20), PriorityClass `serving`. Migrations are additive (a new
column or table; never a rename in place), so the old and new replica run against the same schema during a roll. The
contract only grows, so pigeon, magpie, the gateway and supervisors of the previous version keep working during a
hermit roll and vice versa (A15.F17). Supervisor protocol: the supervisor reports `protocol`; hermit and magpie accept
the current and previous `hermit-sandbox`/`magpie-sandbox` numbers while sandboxes of the previous image live (at most
9 h, or `POST /v1/admin/recycle` to shorten it; platform 3.4).

---

## 21. Deployment (YYC)

### 21.1 Manifests (`YYC/yyc-ai/deploy/hermit/`, applied by `yyc-ai.yml` on the new cluster; platform 9.1, 9.6)

- `hermit.yaml`: `Deployment hermit` in `yyc-ai` (20.3): container `hermit` (image pinned by `<sha12>`, args `[serve]`,
  ports 8080/9090, env from Secret `hermit` by `secretKeyRef` plus the two fieldRefs, `readOnlyRootFilesystem`,
  `runAsNonRoot` uid 65532, drop all capabilities, RuntimeDefault seccomp, `allowPrivilegeEscalation: false`,
  `enableServiceLinks: false`; volumes: `hermit-config` (ConfigMap, hashed) at `/etc/hermit/config`, `catalogue`
  (ConfigMap, not hashed, no `subPath`) at `/etc/hermit/catalogue`, PVC `hermit-jfs` at `/jfs` with
  `mountPropagation: HostToContainer`, `/tmp` emptyDir 256 Mi; required node affinity to nodes in network `yyc`
  (label `yyc.hetzner/network=yyc`, any eu-central location, fsn1 preferred; DESIGN 15.7; they carry the overlay route and the JuiceFS CSI node plugin); a hostname spread with `DoNotSchedule`;
  resources
  platform 16.1). `Service hermit` port 80 -> 8080. `ServiceAccount hermit`, `Role hermit-read`, `RoleBinding`
  (18.5). `PodDisruptionBudget hermit` (minAvailable 1). NetworkPolicies `hermit-in` and `hermit-out` exactly as
  platform 10.2: in from pigeon, magpie, egress, tools, Jobs `app=import` and `yyc/monitor` on 8080; out to magpie
  8080, egress 8082, the API server (`CiliumNetworkPolicy` `hermit-out-api`, `toEntities: [kube-apiserver]`, 6443;
  DESIGN 15.2 N3; it carries the EndpointSlice watch of 13.8), the fabric `10.202.0.0/24:7443`, the sandboxes
  `10.203.0.0/16:7070`, INTERNET 443 (EC2 API), Postgres and DNS (namespace-wide policies). **hermit is the only pod
  in `yyc-ai` whose egress policy allows `10.203.0.0/16:7070`**; magpie's does not (3.10). Replaces
  `web-to-sandbox-hosts`, `web-to-apiserver`, `web-from-ui`, `web-from-news-monitor` (A3.F59, A0.F40).
- `fabric.yaml` (namespace `yyc-ai-fabric`): the `hermit-gateway` DaemonSet and ConfigMap (14.2), the
  `hermit-overlay-route` DaemonSet (on every new-cluster node in network `yyc`: routes 10.202.0.0/15 via 10.0.0.1 in
  the host namespace and sets `rto_min 20ms` for
  10.203.0.0/16 in each ready egress pod's netns, A7.F33), and the one-off Job `hermit-jfs-owner` while it is needed
  (15.4).
- `juicefs.yaml`: static PV `hermit-jfs` (`volumeHandle yyc-ai-hermit`, mount options `cache-size=0`, `open-cache=0`,
  `all-squash=1000:1000`) and PVC `yyc-ai/hermit-jfs` (platform 9.6 plus the squash option).
- Changes elsewhere (platform 10.2, 25.3): `egress-in` admits 10.203.0.0/16 on 3128; on the old cluster
  `db/pg-ingress` admits 5432 from 10.0.0.0/16, which covers the hosts' JuiceFS metadata arriving from the gateway
  node's address (A19.F17) and hermit's own connections arriving from its node's; on the new cluster `db/pg-shared`
  (selector-less, EndpointSlice 10.0.0.2:30432, NodePort 30433) is the cluster-local name of `pg` (30).

### 21.2 Secrets (Sealed Secrets from store `yyc`, `infra/seal.py`; platform 12.2)

| Sealed Secret | Keys (store key) |
|---|---|
| `yyc-ai/hermit` | `HERMIT_DATABASE_URL` (`YYC_HERMIT_DATABASE_URL`), `HERMIT_TOKEN_KEYS` (`YYC_HERMIT_TOKEN_KEYS`), `HERMIT_ACCEPT_PIGEON` (`YYC_TOKEN_PIGEON_HERMIT`), `HERMIT_ACCEPT_MAGPIE` (`YYC_TOKEN_MAGPIE_HERMIT`), `HERMIT_ACCEPT_GATEWAY` (`YYC_TOKEN_GATEWAY_HERMIT`), `HERMIT_ACCEPT_TOOLS` (`YYC_TOKEN_TOOLS_HERMIT`), `HERMIT_ACCEPT_MONITOR` (`YYC_TOKEN_MONITOR_HERMIT`), `HERMIT_ACCEPT_ADMIN` (`YYC_TOKEN_ADMIN_HERMIT`), each `…\|…_NEXT?` for rotations; `HERMIT_TOKEN_MAGPIE` (`YYC_TOKEN_HERMIT_MAGPIE`), `HERMIT_TOKEN_EGRESS` (`YYC_TOKEN_HERMIT_EGRESS`), `HERMIT_FABRIC_*` (14.5), `HERMIT_AWS_*` (14.4), `HERMIT_JUICEFS_META_URL` (`YYC_HERMIT_JUICEFS_META_URL`), `HERMIT_JUICEFS_META_PASSWORD` (`YYC_PG_JUICEFS_PASSWORD`) |
| `yyc-ai-fabric/hermit-gateway` | `host.crt`, `host.key` (`YYC_HERMIT_FABRIC_GATEWAY_CERT/KEY`), `ca.crt` (`YYC_HERMIT_FABRIC_CA`), `wg.key` (`YYC_HERMIT_GATEWAY_WG_KEY`) |
| `yyc-ai/hermit-juicefs` | `name`, `metaurl` (`pg-shared.db`, no password), `envs` `{"META_PASSWORD": …}` (`YYC_JUICEFS_*`) |
| `yyc-ai/hermit-import` | `HERMIT_IMPORT_JARVIS_URL` (`YYC_JARVIS_DATABASE_URL`, read-only use) |
| `db/pg-role-hermit` | the `hermit` role's password (`YYC_PG_HERMIT_PASSWORD`) |
| `yyc/monitor-keys` | `HERMIT_OPS_TOKEN` (= `YYC_TOKEN_MONITOR_HERMIT`), replacing `JARVIS_OPS_TOKEN` |

Each new key is made with 32 random bytes and written with `scripts/store-put.py yyc KEY=file:PATH --explain KEY=…`
(platform 12.3). Every pair token is sealed on both sides. A per-Deployment `yyc/secrets-rev` annotation rolls only the
readers of a resealed Secret (gap-2 2.15). Rotation of any pair token is platform 3.7's three rolls (zero downtime;
replaces the single-valued `JARVIS_CC_TOKEN`, gap-2 2.16). Removed in the teardown: `web-env`, `sandbox-vm`, and
`JARVIS_CC_TOKEN`/`JARVIS_SENT_FILES_TOKEN` everywhere (A7.F28-F31, A19.F6).

### 21.3 Database and backups

`news-stream/deploy/db/postgres.yaml`: managed role `hermit` and `Database hermit` (owner `hermit`); `pg-dump-daily.yaml`
adds `hermit:hermit:HERMIT` (platform 11.1-11.2; A19.F15, A19.F16).

### 21.4 The news-stream monitor (A7.F38, A19.F0-F6, A19.F25)

In the switch commit (cutover P3, platform 18.2 commit B): `monitor.py` reads `HERMIT_OPS_URL` (default
`http://hermit.yyc-ai.svc.cluster.local/ops/snapshot?tz=Asia/Hong_Kong`) and `HERMIT_OPS_TOKEN` (required), sends only
`Authorization: Bearer`, and keeps `ai_slim` and `ai_activity` unchanged (18.3 keeps both contracts; platform 15.5);
`monitor.yaml` env and its `yyc/secrets-rev` bump; NetworkPolicy `hermit-in` admits it. The news-stream test fixtures
follow in the same commit: `tests/test_live_dashboard_page.py` (the snapshot fixture at line 131 uses `kind: "hub"`
and host `yyc-sbx-gw`, which become `kind: "gateway"`, `hermit-gw`; its deployments list `web`, `ui` becomes `pigeon`,
`magpie`, `hermit`), and the other A19.F25 fixtures that fake the snapshot.

### 21.5 The catalogue's readers, and the one commit that moves them (A7.F3, A7.F5, A19.F8, A19.F12, A19.F18, A19.F23)

`catalogue.json` exists from cutover P1 beside `config.json`, with a CI check (cutover P0) that its `users` and
`admins` equal `config.json`'s while both exist. **Every reader moves to `catalogue.json` in the switch commit** (the
"cut commit": cutover P3, platform 18.2 commit B, one YYC push with its tofu change), and `config.json` itself is
deleted with `legacy/` in cutover P5.4, read by nothing but the draining old stack until then:

| Reader | Change in the switch commit |
|---|---|
| ConfigMap `yyc/yyc-catalogue` (yyc-mcp Deployment and CronJob `mcp-cleanup`, `news-stream/deploy/apps/yyc-mcp.yaml`, Kustomization `yyc`) | the generator sources the new file under the **old key**: `files: [config.json=catalogue.json]`, still `disableNameSuffixHash: true`. The object, its key `config.json` and the path `/etc/yyc-catalogue/config.json` do not change, so `CATALOGUE=/etc/yyc-catalogue/config.json` in Deployment `yyc-mcp` (line 39) and CronJob `mcp-cleanup` (line 119) stays as it is, no yyc-mcp Pod rolls, and the kubelet swaps the file content in place (the users and admins are identical; only `config.json`'s other keys disappear, which yyc-mcp never read). Because the key never disappears, no running yyc-mcp Pod or `mcp-cleanup` run can find the file missing, whatever order Kustomizations `yyc-ai` and `yyc` reconcile in. |
| ConfigMap `yyc-ai/catalogue` (hermit, tools) | already from `catalogue.json` (key `catalogue.json`) since P1 |
| tofu `news-stream/deploy/cloudflare/tofu/access.tf:58` | `jsondecode(file("${path.module}/../../../../yyc-ai/deploy/catalogue.json"))` (same `.users`, `.admins` rules; the comment at line 56 follows) |
| `.github/workflows/cloudflare.yml` (path filter line 13, comment line 2) | `yyc/deploy/catalogue.json` |
| `.github/workflows/mcp-image.yml:62` (the yyc-mcp image smoke test) | mount source `$PWD/yyc-ai/deploy/catalogue.json:/etc/yyc-catalogue/config.json:ro` (the container path and `CATALOGUE` stay) |
| `news-stream/mcp-edge/test/catalogue.test.ts:26` (reads the real file) | `../../../yyc-ai/deploy/catalogue.json` |

Texts that name `config.json` as the catalogue, updated in the same commit: `news-stream/mcp-edge/README.md` (lines 5,
36, 81, 155-156), `news-stream/mcp-edge/src/catalogue.ts:1-3`, `src/pages.ts:76` (the call-log page's sentence),
`src/env.ts:12`, `news-stream/wisers/relay.py:6`, `news-stream/deploy/apps/wisers.yaml:3-4`,
`news-stream/deploy/apps/yyc-mcp.yaml:2`, `news-stream/infra/mcp-access.py:7`, `news-stream/tests/test_mcp_edge.py:11`,
`news-stream/deploy/clusters/hetzner/flux-sync.yaml:3`, `news-stream/ARCHITECTURE.md:32`, `news-stream/README.md:1540`,
`news-stream/deploy/README.md:155, 208`, `yyc-ai/README.md:259`. The P5.4 deletion of `config.json` then touches no
reader (cutover P5.4 and its tofu step must not move the catalogue again; 28).

### 21.6 Images and CI (DE0CH/aviary, platform 5-6)

aviary's `ci.yml` runs hermit's fmt, clippy `-D warnings`, unit tests, the integration suite (24.2) against Postgres
17 and fake sandboxd/EC2/k8s/magpie/egress, the conformance checks (24.1), sandboxd's gates (14.6), the sandbox image
build with the supervisor from the same commit and its smoke test (17), and on `main` pushes
`ghcr.io/de0ch/hermit:<sha12>`, `ghcr.io/de0ch/sandboxd:<sha12>`, `ghcr.io/de0ch/hermit-sandbox:<sha12>` and the
release `aviary-<sha12>`. Every package is public (platform R21; the AWS hosts' `crane` pulls anonymously).

---

## 22. Migration and the cut

### 22.1 What is not migrated

Live sandboxes: they run `agent.mjs` with the old `/proc` protocol, which magpie and the supervisor do not speak. They
stay on the old fabric under `web` until the old UI has drained, then go with it. No conversation loses anything:
`/workspace` is the same `ws/<id>` on the same JuiceFS volume, and the conversation is in magpie.

### 22.2 Parallel fabric (why there is no handover)

`web`'s scaler and hermit's scaler would fight over one gateway (`PUT /v1/peers` sets the whole peer set), and each
would terminate the other's EC2 instances as orphans (unknown to its database, older than 10 min). So hermit gets its
own gateway, ranges, PKI, template, tags and IAM user (13.1, 14), and neither controller ever sees the other's hosts.
Both mount the same JuiceFS volume. During the overlap (at most the old UI's 15-minute drain) one conversation could
have a sandbox in each fabric if a staff member keeps an old tab open; both see the same `/workspace` (JuiceFS is
consistent), which is the same situation as two sandboxes of one person sharing `~/.claude` today. vCPU quota: old and
new together stay within 4 m7i.xlarge (14.4); a launch over the quota fails as `launch-failed` and the start ends
`sandbox_busy`.

### 22.3 Order (hermit's part of the switch; cutover phases in brackets)

1. **Before, any time [P1], after the new cluster's T1-T2 (DESIGN 15.2):** the hermit database and role (on `pg`,
   old cluster); PKI, IAM, security group, template and AMI (`infra.sh`); the gateway prerequisites (14.2) and the
   `hermit-gateway` DaemonSet on the new cluster's gateway node; the overlay route, NetworkPolicy, `pg-shared` and
   `pg-ingress` additions; hermit deployed (2 replicas) on the new cluster. hermit is live and idle: its scaler manages only its gateway;
   it launches nothing until a start or a hint asks (spareSlots 0).
2. **Ownership fix-up [P2, before the first import]:** Job `hermit-jfs-owner` (15.4).
3. **Import [P2 onwards]:** `hermit import-jarvis` (CronJob `hermit-import-jarvis` in `yyc-ai` every minute,
   `concurrencyPolicy: Forbid`, `--delta` after the first run, as cutover 8.1 says), with a read-only connection to
   `jarvis` (`HERMIT_IMPORT_JARVIS_URL`); idempotent by source id, `imported = true`:
   - `sandboxes` rows with `owner is not null` and `status = 'gone'`, released in the last 400 days -> `sandboxes`
     (person = owner, `name` = pod, `host`, `ip`, timestamps and `release_reason` as they are, `release_code` derived
     from the reason by 13.13's mapping of the old strings, `token_kid` null: they are dead). The 75 owner-null rows from
     before Claude Code in the sandbox are dropped (A13.F11).
   - `ops_events` kind `sandbox_start` of the last 30 days -> `sandbox_starts` (`source_id`; kind `view_as` is magpie's
     import; the 13 legacy `budget` rows are dropped).
   - `vm_events` -> `host_events` (all 428, keeping kinds, `source_id`).
   - `vm_hosts` with `status = 'gone'` -> `hosts` (kind `hub` -> `gateway`), so "hosts gone" history continues. The live
     old hosts are not imported (they are `web`'s until the end).
   - Every `at` keeps its source value (the monitor's cursor rule, 18.3).
   - Shares' frozen workspaces: every `users/<key>/shares/<shareId>/ws` -> `juicefs clone` into `ws/<uuidv5(SHARE_NS,
     "share:"+shareId)>`, then ensure (quota); an existing target is skipped. magpie's import computes the same id
     (3.8; magpie 19.1, cutover 7.4.4). The other files of those share folders, `users/<key>/claude` and `sent/` are
     read by magpie's own import, not by hermit.
4. **Switch [P3]:** the front door moves to pigeon, magpie starts using hermit; the monitor and every catalogue reader
   move (21.4, 21.5). Nothing to do inside hermit.
5. **After the old UI has drained (and `web` is deleted) [P5]:** the old fabric's teardown script
   (`aviary/fabric/aws/legacy-down.sh`, run once): terminate EC2 instances tagged `app=yyc-ai-sandbox,
   jarvis=ai.yyc.deyaochen.com`; stop and disable `sandboxd` and `sbx-jfs` on `yyc-pool-apps-worker2` (old cluster;
   the server is then free for the Talos session's T2 rebuild), delete `wg0`, the
   nft table `ip sbxhost`, the ip rules and table 51822, `/etc/sbx`, `/var/lib/sbx`; delete the Hetzner route
   10.200.0.0/15 and firewall `yyc-sbx-gw`; delete launch template `yyc-ai-sandbox-host`, its AMIs and snapshots, IAM
   users `yyc-ai-sandbox-scaler` and `yyc-ai-sandbox-ami-ci`, security group `yyc-ai-sandbox-host`. Then the last delta
   import, the second `hermit-jfs-owner` run (15.4), and the git deletions (cutover P5.4: `legacy/`, `yyc-ai-ami.yml`;
   jarvis3 `chat/src/web`, `chat/sandboxd`, `chat/sandbox`, platform 17.1).
6. **30 days after the switch [P6]:** `hermit legacy-cleanup --confirm` removes `users/` and `sent/` from the JuiceFS
   volume (magpie's import holds their contents) and the empty `ws/` folders that magpie no longer references (it asks
   magpie `POST /v1/hermit/workspaces/exists {ids}` in batches of 1 000, magpie 9.7, and skips any id with a live
   sandbox). Until then they are the rollback source.

### 22.4 Done by other components for hermit's sake

- egress: a `person` column on its log rows from the verified token, and a one-time backfill for the historic rows
  (sandbox -> owner from `jarvis.sandboxes`, else the legacy `conversations.owner`) before the legacy tables are dropped
  (A13.F5); `GET /v1/ops/summary` (18.4); token verification and liveness lookups as 6.3; no more `kv egress_pod:*`; no
  :8081 listener (6.5).
- tools: the gateway's owner header or 6.3's verification (6.4); its upstream listing identity from its own config, not
  `admins[0]` (gap-2 2.1).
- magpie: `GET /v1/ops/people`, `GET /v1/ops/view-as` (18.4), `POST /v1/hermit/workspaces/exists`, `POST
  /v1/hermit/persons/{person}/purge`, the view-as and share imports, the workspace id convention (3.8), calling
  `release` on soft delete, permanent delete and purge, and `DELETE /v1/workspaces/{id}` on permanent delete, purge and
  share revoke.

### 22.5 Rollback

Before step 5, the old stack is intact (front door back to `ui`, which still has `web` and the old fabric). After step
5 there is no old fabric to go back to; a rollback of hermit is a previous hermit image (the schema only grows).

---

## 23. Sizes, limits and timeouts

| What | Value |
|---|---|
| JSON request body | 1 MiB (413) |
| Workspace read | streamed, up to the workspace's 1 GiB; 64 concurrent per replica |
| Descriptor (`/v1/sandbox/self`) | <= 64 KiB; env + boot env <= 128 entries / 16 KiB (sandboxd's spec limits, A5.F18) |
| Long-polls (`GET …/sandbox`, `/v1/sandbox-events`) | `waitMs` <= 25 000; held requests <= 2 000 per replica (503 `unavailable` above) |
| Feed page | 1..500 events (default 200); retention 7 days |
| Sandbox token | about 180 characters; Ed25519; valid to the hard lifetime (9 h) |
| Liveness lookup | in-memory, p99 < 1 ms; gateway cache 30 s, stale served without limit while hermit is unreachable; lookup timeout 1 s |
| Token keys | gateway refresh 5 min; unknown kid refetch at most every 10 s |
| `PUT …/sandbox` | p99 50 ms; reservation 120 s |
| Health check of a supervisor | 2 s timeout |
| Activity reports | every 15 s; silence 90 s + 2 unanswered checks 5 s apart -> `unhealthy` (paused while the gateway is down and for 120 s after) |
| Start | capacity wait 60 s; host wait 170 s; sandboxd create 15 min call timeout; supervisor wait 120 s; overall 480 s |
| Row claims | 15 s, renewed every 5 s; takeover sweep every 2 s |
| Release | supervisor deadline 30 s (+5 s); a refused soft release restarts the idle clock; stuck releasing 10 min |
| Lifetimes | idle 30 min; max 8 h; grace 60 min (hard 9 h); pool victim idle >= 120 s and no busy report for 90 s |
| Pool | `maxActive` 10 live sandboxes; 12 slots per m7i.xlarge; `maxHosts` 3 (+1 transient) |
| Sandbox | 1 CPU, 1 GiB memory, 4 096 pids, 65 536 open files, 5 GiB scratch, `/dev/shm` 64 MiB, `/workspace` 1 GiB / 1 000 inodes (gap-0 1-2) |
| Scaler pass | 5 s; host dead 30 s; gateway dead 120 s; gateway settle 120 s; boot timeout 480 s; orphan instance 600 s; JuiceFS retire 120 s; host minimum life 900 s; demand window 120 s |
| Migration | downtime 100 ms, 20 rounds, abort above 500 ms, min age 30 s, idle CPU 0.05; send/receive 15 min; receive wait 90 s |
| Janitor | 30 s; retention daily |
| Ops snapshot | cache 4 s per zone; section timeout 8 s; magpie/egress calls 5 s |
| Catalogue | re-read 30 s; <= 1 MiB, <= 10 000 entries |
| Postgres pool | 10 per replica + leader + listener; statement timeout 15 s; no transaction across I/O |
| hermit pod | platform 16.1 (100m / 1 CPU, 384 Mi); expected RSS 40-80 MiB |
| Drain | preStop 10 s; grace 90 s |

---

## 24. Tests (the acceptance bar)

### 24.1 Conformance (`aviary-conformance hermit`, any implementation; platform 8.1)

Runs against a live hermit with a fake fabric: every route's shapes and error codes (camelCase, `/v1/` paths);
idempotent `PUT` from two clients at once yields one sandbox; long-poll wakes on change and times out with the same
version; feed order, `latest`, 410 after retention; release idempotent; pause/resume/snapshot either work or answer
501 `not_supported` consistently (the suite's `--capabilities` argument says which the implementation under test
claims; the implementation itself publishes no flags); `/v1/sandbox/**` only with the gateway token and matching
headers; the descriptor's fields and `etag`/304; the activity reply's `release`; secrets 404 `not_granted` when not
granted; token keys published and a minted token verifying with them; liveness `live:false` for gone or not-allowed;
people rules (`@domain`, admin implies allowed, case and whitespace).

### 24.2 hermit integration tests (CI, ported from jarvis3 `web.test.ts`, `vm.test.ts`, `packing.test.ts`, `ops.test.ts`, `common.test.ts`; A20.F11)

- Auth: each caller token only on its routes (403), missing token 401, `Aviary-Admin` required, unknown route 404.
- Lifecycle: one sandbox per conversation; the same one while it lives; `PUT` resets idle and reserves; activity resets
  idle; busy is never idled; the janitor releases idle ones and the next `PUT` makes a new one; release on delete; soft
  delete releases.
- **Release refusal**: an idle-release asked of a fake supervisor that answers 409 leaves the sandbox `ready` and
  restarts its idle clock; a pool victim that refuses is skipped; forced releases send `force: true` and destroy after
  200 or the deadline; the activity reply carries a pending release; a `PUT` during a pending soft release cancels it.
- **Admission race**: a sandbox idle for 29:59 is `PUT` and is not released before its reservation ends or a busy
  report arrives; a pool start never picks a reserved sandbox.
- Pool: full pool frees the least-recently-active idle sandbox, never a busy one; with none, `sandbox_busy` after 60 s
  (not 500).
- Lifetime: 8 h defers while busy; 9 h releases forced (the fake supervisor gets `force: true`).
- **Health**: `ok:true, ready:false` never counts as unhealthy; a silent sandbox is released only after two
  unanswered checks; with the gateway host not answering (or within 120 s of its return) the silence rule does nothing.
- Resumable tasks: kill the replica after each start phase in turn, during a release, and at each migration step;
  another replica completes it; tokens identical. **Migration crash window**: the source's send succeeds and the
  replica dies before recording `host = to`; the takeover adopts the sandbox on the target; it is never `vanished`; the
  target copy is never swept as an orphan; a drain mid-migration hands it over the same way.
- **No long transactions**: with Postgres `idle_in_transaction_session_timeout = 1s` and a fake sandboxd that holds
  create, send and receive for 30 s, every task still completes (proves nothing waits on I/O inside a transaction).
- **Tokens**: a minted token verifies with the published key; tampered payload, wrong kid, expired, wrong source
  address are refused; key rotation (two kids) keeps old tokens valid; liveness answers from memory with Postgres
  stopped.
- Fabric (fake sandboxd + fake EC2): placement on the fullest host; burst to AWS; drain off retiring hosts by migration;
  empty host termination after its minimum life; `maxHosts` cap reported (`sandbox_busy`), not waited out; typing starts
  a host only without a free slot; gateway rename; gateway peers /32 and IPv6; capacity planning counts pending slots;
  slots by memory limit; an unanswering host loses its sandboxes and is terminated; JuiceFS gating; `alive:false` ->
  `vanished` within one pass; orphan instances terminated; host events recorded.
- Packing: fewest hosts; a move only when strictly fewer; least-recently-moved tie-break.
- Egress targets follow EndpointSlices (ready only, port 3128; empty keeps the last list).
- Workspaces: quota on every ensure; path escapes refused (symlink to `/`, `..`, absolute); clone keeps content, sets
  quota and flushes a live sandbox first; delete releases first; everything created is uid 1000; a broken mount answers
  503 and hands starts over.
- Catalogue: hot reload; refused file keeps the last good one; admin not in users refused; `catalogue-check` agrees.
- Ops: admin re-check, monitor token scope, `safeZone`, every section, health verdicts, now/history apart, **the
  monitor's two contracts** (a golden file of the `ai_slim` field paths and of the `ai_activity` paths and value rules:
  `what` in `{start, release}`, `owner` an email, every `at` matching `^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d{3}Z$`,
  compared on every run), imported rows keep their `at`, no message contents.
- Drain: SIGTERM -> not ready, claims handed back and taken over, leadership moves, exit within 90 s.

### 24.3 Fabric acceptance (staging on real hosts, before the switch; recorded in the task record)

- sandboxd soak (14.6) green with the changes of 13.2.
- FUSE in gVisor with the fork's runsc: the supervisor mounts `~/.claude`, Claude Code writes its transcript through it
  (write returns only once magpie stored it); then a **live migration with the FUSE mount active and a turn streaming**:
  no lost write, no stall above 1 s, same PIDs (ported `proto/check-workload.sh` invariants, A5.F49, A20.F14).
- **Migration takeover**: kill the hermit replica running a migration right after `send` returns; the sandbox is
  adopted on the target and the turn continues.
- OOM: a process allocating past 1 GiB kills the sandbox; hermit reports `vanished/sandbox_lost` within 10 s.
- Host loss: terminate an AWS host from the console mid-turn; `sandbox_lost` within 35 s.
- **hermit down**: scale hermit to 0 (by a test-only Flux suspension in staging) during a streaming turn; the turn
  completes (model, tools and FUSE writes through the gateway).
- **root-squash**: as root in a sandbox, `pip install` into `/workspace`, `git clone` into it, `cp -a` a root-owned
  tree; everything lands as uid 1000; hermit then deletes the workspace without EACCES.
- Measured baselines kept or beaten (A20.F22): warm start median <= 2 s; cold start <= 80 s with a fresh AMI; migration
  pause p90 <= 600 ms; sandboxd RSS <= 31.5 MiB.
- Fixed names from inside a sandbox: `curl -x "$http_proxy" http://hermit/v1/sandbox/self` works; without the
  credential 407; with a token from another sandbox's address 403; `http://hermit/v1/people` 403.
- Rollout: a hermit image roll during ten parallel cold starts: all ten become ready.

### 24.4 Local harness (A20.F10)

The platform harness (platform 7) runs hermit's YYC implementation with `testing/e2e/hermit.json` (one fake gateway
host, `maxHosts: 0`), `fake-sandboxd` creating sandbox Pods from the real image, and `test-gateway` (the reference
gateway of 6.3), so the turn path pigeon -> magpie -> hermit -> supervisor runs on a laptop or one CI runner with no
cloud and no model cost.

---

## 25. Observability

- Logs: one JSON line per event (`aviary-core::log`, platform 15.1), fields `msg`, `sandbox`, `conversation`,
  `person_key` (`"u"` + 24 hex of SHA-256 of the email, platform 3.8; no raw emails in logs), `host`, `request`, `ms`,
  `error`. Stable messages: `sandbox start requested`, `sandbox phase`, `sandbox ready`, `sandbox released`, `sandbox
  lost`, `release refused`, `supervisor not reporting`, `task taken over`, `host launched`, `host active`, `host lost`,
  `host terminated`, `migration`, `migration skipped`, `migration failed`, `migration adopted`, `catalogue loaded`,
  `catalogue refused`, `workspace delete refused`, `leader acquired`, `leader released`, `admin action`, `drained`.
- Metrics (`/metrics`): `hermit_sandboxes{state}`, `hermit_sandbox_starts_total{ok,reason}`,
  `hermit_sandbox_start_seconds` (histogram), `hermit_releases_total{reason,kind}`, `hermit_release_refusals_total`,
  `hermit_hosts{kind,status}`, `hermit_host_events_total{kind}`, `hermit_migration_pause_ms` (histogram),
  `hermit_liveness_lookups_total{result}`, `hermit_people_lookups_total{result}`, `hermit_catalogue_refused_total`,
  `hermit_leader{loop}` (1 on the leader), `hermit_task_takeovers_total{task}`, `hermit_scaler_pass_seconds`,
  `hermit_http_requests_total{route,status}`, `hermit_held_requests`.
- Debugging a chat across services (gap-2 2.13): the conversation id is in every hermit log line and every hermit row;
  `GET /v1/admin/sandboxes?conversation=` plus magpie's and egress's admin reads replace SQL on `jarvis` (platform 15.2).

---

## 26. Interfaces summary (for the other specs)

Provided (all at `http://hermit`, bearer per caller, section 3.2; types in `hermit-api`):
`GET /v1/deployment`; `GET /v1/people/{email}`; `GET /v1/people`; `POST /v1/warm`; `PUT|GET
/v1/conversations/{id}/sandbox`; `POST /v1/conversations/{id}/sandbox/{release|pause|resume|snapshot}`; `GET
/v1/sandbox-events`; `PUT|GET|DELETE /v1/workspaces/{id}`, `GET /v1/workspaces/{id}/entries`, `GET
/v1/workspaces/{id}/files`, `POST /v1/workspaces/{id}/clone`; `GET /v1/sandbox/self`, `GET
/v1/sandbox/secrets[/{key}]`, `POST /v1/sandbox/activity` (through the gateway); `GET /v1/sandbox-token-keys`, `GET
/v1/sandbox-tokens/{sandboxId}`; `GET|HEAD /ops/`, `GET|HEAD /ops/snapshot`; `/v1/admin/**`; the sandbox token format
and the boot environment (5.1, 6.1).

Consumed: supervisor :7070 `GET /v1/health`, `POST /v1/lifecycle/release`, `POST /v1/lifecycle/prepare` (snapshot
flush), `POST /v1/exec` (admin only), the keeper entrypoint (5.1) and the activity cadence (3.7.3); magpie `GET
/v1/ops/people`, `GET /v1/ops/view-as`, `POST /v1/hermit/workspaces/exists`, `POST /v1/hermit/persons/{person}/purge`;
egress `GET /v1/ops/summary` and the gateway behaviour of 6.3; sandboxd's API (13.15); EC2; the k8s API (deployments,
pods, endpointslices); JuiceFS (CSI mount, `juicefs` CLI with the metadata URL); Postgres `hermit`; the catalogue file.

---

## 27. Coverage: every inventory feature whose target_home names hermit

220 features: every feature whose `target_home` names hermit (216 rows), plus 13 that review round 1 assigned here (A7.F3, A18.F0, F5, F11, F13, F16-F19, A19.F2, F12, F23, F25). Section = where this spec handles it; disposition = kept / ported / moved / replaced / fixed / dropped.

| Id | Feature | Section | Disposition |
|---|---|---|---|
| A0.F2 | users / @domain allowlist | 3.4, 3.4.1 | moved: hermit answers `allowed` from the live-reloaded catalogue (`@domain` rule kept) |
| A0.F3 | admins | 3.4, 3.11 | moved: `role: admin` from the catalogue; admin implies allowed |
| A0.F4 | userKey derivation | 1.2 | moved to magpie: hermit no longer needs userKey (no ~/.claude folder) |
| A0.F11 | Operator page forwarding /ops/* | 3.11, 18.1 | kept: hermit serves /ops/*, re-checks admin per request |
| A0.F12 | View-as start: POST /api/jarvis/view-as | 1.2, 18.3 | view-as audit moved to magpie; hermit's page shows it and hosts the form |
| A0.F20 | Idle stop sweeper | 12, 4.2 | replaced by hermit's idle rule fed by supervisor activity |
| A0.F26 | HTTP proxy to the local process | 3.2 | replaced: pigeon routes to hermit's REST; no per-person process |
| A0.F34 | POST /api/jarvis/warm (passed through) | 3.5 | kept: `POST /v1/warm` demand hint |
| A0.F40 | NetworkPolicies around ui | 21.1 | replaced by hermit-ingress/egress policies |
| A1.F0 | Jarvis mode switch and the two modes | 1.1 | dropped: no mode switch; behaviour split across services |
| A1.F4 | Request gate: X-Jarvis-User must equal the owner | 3.2, 6.3 | replaced: per-caller pair tokens + `Aviary-Person` / `Aviary-Sandbox-*` headers |
| A1.F7 | Sandbox acquisition per turn | 3.6.2, 3.6.3, 11 | replaced: idempotent `PUT` (with a reservation) + long-poll, resumable start |
| A1.F8 | 'Preparing the workspace' status frames and Stop while preparing | 4.3, 3.6.1 | phases exposed; chip and stop-while-preparing are magpie's |
| A1.F10 | Sandbox keep-alive while Claude Code runs | 3.7.3 | replaced by the supervisor's activity reports |
| A1.F12 | Fixed model: one instance model, no picker, no effort | 1.2, 3.7.1 | model env moved to the supervisor + `model`; hermit carries no model |
| A1.F20 | Permanent delete (force) and its side effects | 3.6.4, 3.8 | hermit part: release + workspace delete, called by magpie |
| A1.F45 | Typing warms a sandbox host | 3.5 | kept |
| A2.F12 | get_providers_snapshot_request (providers/models) | 3.3 | hermit gives product/locale only; model from `model` |
| A2.F23 | send_agent_message_request (send) | 3.6.2 | hermit part: the sandbox for the turn |
| A2.F25 | workspace.create.request (new conversation + first message) | 3.6.2, 3.8 | hermit part: workspace ensure + sandbox for the first turn |
| A2.F32 | yyc.sandbox_status (preparing chip) — protocol addition | 3.6.5, 4.3 | state from hermit's feed/phases; magpie emits the chip |
| A2.F36 | View-as on the Paseo socket | 1.2, 18.3 | audit in magpie; shown on hermit's ops page |
| A2.F39 | REST surface the app uses beside the socket | 3.5, 3.8, 3.11 | hermit routes behind pigeon: warm, ops; files via magpie -> hermit workspaces |
| A3.F0 | POST /internal/cc/sandbox: acquire the conversation's sandbox | 3.6.2, 11 | replaced: PUT + resumable start; busy is sandbox_busy |
| A3.F1 | POST /internal/cc/touch: mark a sandbox in use | 3.7.3 | replaced by activity reports |
| A3.F2 | POST /internal/cc/release: conversation deleted | 3.6.4 | kept as `POST /v1/conversations/{id}/sandbox/release` |
| A3.F3 | POST /internal/cc/warm: typing hint for the scaler | 3.5, 13.5 | kept; demand_hints table instead of kv |
| A3.F5 | GET /healthz: readiness and liveness | 20.1 | kept on port 9090; readiness no longer depends on Postgres |
| A3.F6 | Bearer auth of internal routes (JARVIS_CC_TOKEN) | 3.2 | replaced: one token per caller, lists for rotation |
| A3.F7 | HTTP plumbing limits and error shape | 3.1, 23 | kept: 1 MiB JSON, JSON errors with codes; PoolFull -> 503 sandbox_busy |
| A3.F8 | Zero-downtime drain on SIGTERM | 20.2 | changed: drain hands starts over instead of waiting 300 s |
| A3.F9 | Advisory-lock leadership for singleton loops | 10.3 | kept with hermit lock keys |
| A3.F10 | Janitor pass | 12, 13.4 | kept; vanished moved to the 5 s scaler pass |
| A3.F11 | Sandbox lifecycle rules and limits | 4.2 | kept; idle and soft lifetime refused while busy; hard 9 h forced and orderly |
| A3.F12 | user_key derivation (~/.claude folder key) | 1.2 | moved to magpie |
| A3.F13 | Tokens per sandbox | 6.1, 6.2 | replaced: signed sandbox token verified by the gateway; derived control token held by hermit only |
| A3.F14 | Sandbox env: no secrets in a sandbox | 3.7.1, 5.1, 7 | kept: proxy boot env and mirror env only, no secrets |
| A3.F15 | Claude Code environment in the sandbox (claudeCodeEnv) | 1.2, 3.7.1 | split: Claude Code env to the supervisor; hermit gives only internet env |
| A3.F16 | Managed CLAUDE.md = instance system prompt + SANDBOX_RULES | 3.7.1, 9 | kept as data: prompt + rules files rendered with real quotas |
| A3.F19 | Provider abstraction | 8.1 | dropped as a switch; one fabric, a test-only trait |
| A3.F20 | Provider kubernetes (gVisor pods) | 8.1 | dropped |
| A3.F21 | Provider e2b (hosted Firecracker sandboxes) | 8.1 | dropped |
| A3.F22 | Provider vm: create (placement + sandboxd POST) | 11.2, 13.3 | ported; no transaction across the sandboxd call |
| A3.F23 | Provider vm: destroy and list | 13.12 | ported as soft/forced/lost releases; no legacy provider |
| A3.F24 | vm scaler pass (every 5 s, leader lock 4242012) | 13.4 | ported |
| A3.F25 | Host heartbeat / JuiceFS gating / CPU features | 13.4 | ported |
| A3.F26 | Host loss handling | 13.7 | ported; magpie told via the feed |
| A3.F27 | Capacity planning and AWS launch | 13.5 | ported |
| A3.F28 | AWS host release rule | 13.6 | ported; emptyMinutes removed |
| A3.F29 | EC2 reconcile | 13.9 | ported with hermit's own tags |
| A3.F30 | Network push: hub WireGuard peers, mesh peers, egress targets | 13.8 | ported; egress targets from EndpointSlices |
| A3.F31 | Live migration (pre-copy, gVisor fork) | 13.10 | ported; intent recorded, resumable, takeover adopts a finished move |
| A3.F32 | Packing planner (pure) | 13.11 | ported as is |
| A3.F33 | Slot accounting | 13.3 | ported |
| A3.F34 | Image digest resolution and warm-up | 13.14 | simplified: image pinned by digest |
| A3.F35 | Operator knobs (SQL only) | 16 | replaced by admin REST with audit |
| A3.F36 | Legacy provider during a provider switch | 8.1, 22.2 | dropped; parallel fabric instead |
| A3.F37 | GET /internal/ops/ — operator page HTML | 18.2 | ported into hermit (static page) |
| A3.F38 | GET /internal/ops/snapshot?tz=<zone> — operator data | 18.3, 18.4 | kept field names; aggregates hermit + magpie + egress |
| A3.F39 | Health verdict (healthOf) | 18.6 | ported with new findings |
| A3.F40 | Ops section: service (Deployments and pods) | 18.5, 9 | kept; role list is hermit config |
| A3.F41 | Ops section: sandboxes, hosts, hostsGone, scaler, events | 18.3 | kept |
| A3.F42 | Ops section: starts and releases | 18.3 | kept from sandbox_starts |
| A3.F43 | Ops section: recent errors (15 min) | 18.3, 18.4 | kept; egress part from egress |
| A3.F48 | ops_events recording | 10.2 | sandbox_start -> sandbox_starts; view_as -> magpie |
| A3.F49 | vm_events recording | 10.2 | -> host_events |
| A3.F50 | Postgres schema migrations (shared by all roles) | 10.2, 22.3 | split: hermit's own DB and migrations; legacy tables dropped |
| A3.F51 | kv table usage | 13.8, 1.2 | dropped: typed tables, EndpointSlices |
| A3.F52 | Config loading and validation | 9 | replaced by hermit.json (strict) + catalogue |
| A3.F55 | Postgres pool settings | 10.1 | kept: 12 per replica |
| A3.F56 | Sandbox agent client (health) | 3.10 | replaced by the supervisor's `GET /v1/health` (`ready` flag) |
| A3.F57 | sandboxd client (mTLS) | 13.15 | ported (CN hermit) |
| A3.F58 | WireGuard key generation for new hosts | 13.5 | ported |
| A3.F59 | Network policies and RBAC for web | 18.5, 21.1 | ported |
| A3.F60 | Container hardening and resources | 21.1 | ported (Rust, uid 65532, read-only) |
| A3.F61 | Secrets/env consumed by web | 8.4, 21.2 | replaced by HERMIT_* secrets |
| A4.F31 | yyc.sandbox_status chip | 4.3 | kept (state source) |
| A4.F32 | Warm a sandbox host while typing | 3.5 | kept |
| A5.F0 | Sandbox image base and toolchain | 17 | kept; supervisor replaces the agent |
| A5.F1 | reportlab CJK font redirect | 17 | kept |
| A5.F2 | Docker inside the sandbox (on demand) | 17 | kept; dockerd started by the supervisor through `/run/supervisor/docker.sock` |
| A5.F4 | apt/apt-get wrapper + sudo policy | 17 | kept |
| A5.F5 | Home, PATH and env defaults | 17 | kept |
| A5.F6 | /opt/image-setup (self-describing image) | 17 | kept |
| A5.F7 | sbx-init: managed Claude Code files from env | 3.7.1 | replaced: the supervisor reads `/v1/sandbox/self` |
| A5.F11 | Exec token auth | 6.2 | replaced by the control token (only its hash in the sandbox; only hermit holds it) |
| A5.F16 | Agent POST /init (e2b provider) and wire proxy | 8.1 | dropped |
| A5.F18 | Sandbox environment contents (no secrets) | 3.7.1, 5.1 | kept: proxy boot env + mirror env only; no identity variable besides the proxy credential |
| A5.F19 | sandboxd mTLS API listener | 13.2, 13.15 | kept |
| A5.F20 | GET /v1/host | 13.4 | kept |
| A5.F21 | Listed sandbox shape + per-sandbox CPU sampling | 13.2 | kept + `alive` |
| A5.F22 | POST /v1/sandboxes (create, idempotent) | 13.2, 11.2 | kept; args from image, no user/quota |
| A5.F23 | DELETE /v1/sandboxes/{name} | 13.12 | kept |
| A5.F24 | GET /v1/sandboxes, GET /v1/sandboxes/{name}/spec | 13.10 | kept |
| A5.F25 | Migration target: POST/GET /v1/sandboxes/{name}/receive | 13.10 | kept |
| A5.F26 | Migration slot threads (finish a receive) | 13.10 | kept |
| A5.F27 | Migration source: POST /v1/sandboxes/{name}/send | 13.10 | kept |
| A5.F28 | migrate-relay helper process | 13.10 | kept |
| A5.F29 | Host storage per sandbox (scratch + overlay root) | 13.2 | kept |
| A5.F30 | Pre-made upper dirs for bind-mount paths | 13.2 | kept (~/.claude dir is the FUSE mountpoint) |
| A5.F31 | JuiceFS persistent folders + quota | 13.2, 15 | ~/.claude bind dropped; ws bind kept; quota moved to hermit |
| A5.F32 | OCI bundle per sandbox | 13.2 | kept; the keeper is the first process |
| A5.F33 | runsc global flags | 13.2 | kept (+FUSE if needed) |
| A5.F34 | Per-sandbox network: netns, veth, nft lock | 13.2 | kept; VIP:3128 only, no VIP:80 |
| A5.F35 | Host-level network (hub / burst / gateway) | 13.1, 14.2 | kept; new gateway via DaemonSet |
| A5.F36 | Route watchdog (every 10 s) | 13.2 | kept |
| A5.F37 | PUT /v1/peers | 13.8 | kept |
| A5.F38 | PUT /v1/egress (hub/gateway) | 13.8 | kept; targets from EndpointSlices |
| A5.F39 | POST /v1/images + image cache | 13.14 | kept |
| A5.F40 | Image server :7444 (hub/gateway) | 13.5 | kept (gateway :7444) |
| A5.F41 | State files and restart recovery | 13.2 | kept |
| A5.F42 | Per-name operation locks and fixed tables | 13.2 | kept |
| A5.F43 | sandboxd fixed memory budget | 13.2, 14.6 | kept with its soak gate |
| A5.F44 | cgroup layout and OOM protection | 14.1 | kept |
| A5.F45 | Host command runner | 13.2 | kept |
| A5.F46 | Gateway mode | 14.2 | kept; installed by Flux |
| A5.F47 | Host image bake / setup (burst AMI) | 14.1, 14.3 | ported to `fabric/`; YYC constants and pins from hermit.json |
| A5.F48 | PKI | 14.5 | new PKI + rotation runbook |
| A5.F49 | Live migration invariants (what must survive) | 13.10, 24.3 | kept; FUSE + supervisor added to the acceptance |
| A5.F50 | Logging | 25 | kept for sandboxd; hermit JSON logs |
| A5.F51 | Test tooling and prototypes | 14.6, 8.1 | soak/fakes kept; proto/ not moved |
| A6.F0 | Egress token identity (sandbox lookup) | 3.9, 6.3 | replaced: signed token verified locally by the gateway; liveness by REST, cached, stale on error |
| A6.F1 | Source-IP binding of vm sandbox tokens | 6.1, 6.3 | kept: the source address is inside the signed token, enforced by the gateway |
| A6.F12 | Route `claude` (model route to OpenRouter) | 1.2 | env wiring moved to the supervisor (`model` fixed name) |
| A6.F23 | Operator page data from egress_log | 18.4 | egress serves its own ops data, records the person |
| A6.F25 | Egress pod announcement for the VIP translation (kv egress_pod:*) | 13.8 | replaced by EndpointSlices |
| A6.F29 | tools: caller identity from egress token | 6.4, 3.9 | tools trusts the gateway header or verifies the token itself |
| A6.F39 | Sandbox env pointing at egress (proxy, mirrors, Claude Code, MCP, hooks) | 3.7.1, 9 | internet env from hermit; Claude Code/MCP env supervisor's |
| A7.F3 | Access catalogue ConfigMap yyc/yyc-catalogue (unhashed) | 21.5 | kept: same object, same key `config.json`, sourced from catalogue.json in the switch commit |
| A7.F4 | config.json: product, publicUrl, access | 3.3, 9 | moved into hermit.json, served by `/v1/deployment` |
| A7.F5 | config.json: users / admins | 3.4.1, 21.5 | moved to catalogue.json; every reader moves in the switch commit |
| A7.F7 | config.json: sandbox (provider vm, image, limits) | 9 | moved into hermit.json |
| A7.F11 | Managed CLAUDE.md / system prompt | 9, 3.7.1 | kept as hermit data |
| A7.F20 | web Deployment (sandbox manager) zero-downtime | 20 | ported with spread + handover drain |
| A7.F21 | Advisory-lock singletons and migrations (Postgres) | 10.3 | kept |
| A7.F22 | Janitor background job | 12 | kept |
| A7.F23 | vm scaler and packing (background job) | 13.4-13.11 | kept |
| A7.F28 | SealedSecrets and the fenced applier | 21.2 | kept mechanism, new names |
| A7.F29 | Secret web-env | 21.2 | replaced by Secret hermit |
| A7.F30 | Secret sandbox-vm | 21.2, 14.4, 14.5 | replaced (new PKI, new IAM user) |
| A7.F31 | Secrets egress-env, tools-env, ui-env, ui-juicefs, tunnel, ghcr-pull | 21.2 | hermit's parts; others each service's |
| A7.F33 | Overlay route DaemonSet sbx-overlay-route | 21.1 | becomes hermit-overlay-route with new ranges |
| A7.F34 | Gateway yyc-sbx-gw (outside Kubernetes) | 14.2, 22.3 | new gateway hermit-gw in GitOps; old removed |
| A7.F37 | Operator page /ops (data sources) | 18 | kept |
| A7.F38 | News-stream live dashboard reads the ops snapshot | 21.4, 18.3 | kept field names; URL/token change in the same commit |
| A7.F40 | Postgres databases and roles for YYC AI | 10.1, 21.3 | new DB hermit |
| A7.F47 | jarvis3 sandboxd workflow (host daemon release) | 14.6 | moved to aviary `ci.yml`, release `aviary-<sha12>` |
| A7.F50 | yyc-ai-ami workflow (AWS host AMI) | 14.3 | replaced by hermit-ami.yml on the three pins, pinned aviary |
| A7.F56 | E2B provider (prepared, not live) | 8.1 | dropped |
| A8.F0 | Per-user ~/.claude folder on JuiceFS (shared by all of a user's conversations and sandboxes) | 1.2, 13.2 | dropped from hermit (FUSE backed by magpie); hermit names the person in `/v1/sandbox/self` |
| A8.F2 | Host JuiceFS mount (sbx-jfs.service) | 14.1 | kept for /workspace only |
| A8.F28 | Managed Claude Code config written outside ~/.claude | 1.2, 3.7.1 | managed files written by the supervisor; hermit gives the CLAUDE.md text |
| A9.F3 | Background task tracker (task events) | 3.7.3 | supervisor reports busy/backgroundTasks to hermit |
| A9.F7 | New send supersedes a hold (releaseHeldSession and the addSession supersede) | 1 | supervisor/magpie decision; hermit only sees busy |
| A9.F9 | Running list during a hold: GET /api/providers/sessions/running | 3.6.1 | busy exposed on the Sandbox; per-conversation running state is magpie's |
| A9.F16 | Sandbox keep-alive /internal/cc/touch during a hold | 3.7.3 | replaced by activity reports |
| A9.F17 | Janitor idle (30 min) and max lifetime (8 h) vs holds | 4.2, 12, 13.12 | changed: soft releases refused while busy; hard at 9 h, forced and orderly |
| A9.F20 | No logs for hold start/end | 25, 18.3 | busy and backgroundTasks visible on the ops page; supervisor logs holds |
| A10.F16 | /api/scheduled-messages (list/create/delete) + scheduled dispatcher | 1 | dropped (not hermit's) |
| A10.F19 | GET /api/providers/sessions/running | 3.6.1, 18.3 | kept as information: busy per sandbox |
| A11.F12 | settingSources project,user,local: model-writable settings, hooks and CLAUDE.md | 3.7.1 | hermit gives only the CLAUDE.md text; setting sources are the supervisor's |
| A11.F14 | Environment forwarded through X-Proc | 3.7.1 | hermit env = internet only |
| A11.F16 | Sandbox touch while Claude Code runs | 3.7.3 | replaced |
| A11.F17 | Sandbox acquisition before spawn (preparing status) | 3.6.2, 4.3 | kept semantics |
| A13.F5 | Ops page budget.byUser fallback to legacy conversations.owner | 18.4, 22.4 | egress backfills the person |
| A13.F8 | Live kv keys egress_pod:/sandbox_demand: (for contrast) | 13.8, 3.5 | replaced by tables/EndpointSlices |
| A13.F11 | Old-UI sandboxes rows (owner NULL) | 22.3 | owner-null rows dropped at import |
| A14.F1 | Caller check at upgrade only (X-Jarvis-User) | 3.2 | replaced by per-request caller identity |
| A14.F11 | Sandbox 'preparing' chip | 4.3, 3.6.5 | state from hermit |
| A15.F17 | In-sandbox agent version skew (ui as client of old sandboxes) | 20.3, 16 | supervisor protocol reported; recycle admin route |
| A16.F4 | Survival: web replica restart during POST /internal/cc/sandbox | 10.3, 11.2 | fixed: resumable starts (row claims, takeover) |
| A16.F5 | Survival: janitor max lifetime (maxLifetimeHours 8) reached mid-turn | 4.2, 13.12 | fixed: soft at 8 h, forced but orderly at 9 h |
| A16.F6 | Survival: sandbox host loss | 13.7, 3.6.5 | fixed: lost within 35 s on the feed |
| A16.F7 | Survival: live migration mid-stream | 13.10 | kept; supervisor must survive moves (24.3); a replica death mid-move is settled by takeover |
| A16.F12 | Survival: sbx-jfs restart on the sandbox host (juicefs crash) and EDQUOT | 13.4, 15 | ~/.claude off JuiceFS; host gating kept |
| A16.F16 | Readable error mapping shown to staff (zh-HK) | 3.6.1, 13.13 | codes from hermit; pigeon renders |
| A17.F0 | Soft delete into Recently deleted | 3.6.4 | decided: soft delete releases the sandbox |
| A17.F4 | onConversationDeleted side effects | 3.6.4, 3.8, 15.4 | release + workspace delete in hermit as uid 1000; no EACCES leftovers |
| A17.F5 | Web sandbox release on delete | 3.6.4 | kept |
| A17.F15 | Jarvis DB retention (what is pruned, what is not) | 10.6 | explicit retention per hermit table |
| A17.F16 | Ops page 'people' incl. former users | 18.3 | kept from catalogue + magpie |
| A17.F17 | Offboarding: remove an email from config.json users | 3.4.1, 3.9, 12, 16 | access stops within ~2 min, tokens refused within 30 s, sandboxes released; data deleted only by the operator purge |
| A18.F0 | Volume format settings | 15.1 | TrashDays 30, no encryption, bucket yyc-ai-files/yyc-ai, DirStats on; never reformatted |
| A18.F2 | Per-conversation /workspace quota 1 GiB / 1,000 inodes | 15.5, 13.2 | fixed: quota set on every ensure by hermit |
| A18.F3 | Quota is missing on conversations whose first message had an attachment (live: 3 of 37) | 3.8, 15.5 | fixed: every sandbox's workspace is ensured with its quota before create; uploads are written by the supervisor inside it |
| A18.F5 | Top folders ws/ and users/ owned by uid 1000 | 15.4 | replaced: all-squash in hermit, root-squash on hosts, a one-time owner fix-up Job; hermit deletes `ws/<id>` as uid 1000 |
| A18.F11 | Automatic metadata backup (hourly meta dump to the bucket) | 15.3 | kept: taken by hermit's mount pods (always running) or the hosts' sbx-jfs; no mount uses `--no-bgjob` |
| A18.F13 | Client background jobs: session cleanup, slice cleanup, compaction | 15.3 | kept on every mount client |
| A18.F16 | JuiceFS CSI driver HelmRelease | 15.2 | kept unchanged (moved to juicefs-csi.yaml, platform 9.1); hermit has node affinity to its nodes |
| A18.F17 | Static PV/PVC of the whole volume | 15.2, 21.1 | replaced by PV `hermit-jfs` / PVC `yyc-ai/hermit-jfs`; the ui PV goes at teardown |
| A18.F18 | Mount pods (one per UI node, shared by UI pods) | 15.2 | kept as a mechanism, now for hermit Pods |
| A18.F19 | Mount-pod restart / upgrade under a running UI pod | 15.6 | handled: `mountPropagation: HostToContainer`; readiness unchanged; 503 + start hand-over while broken |
| A18.F20 | AWS host sbx-jfs.service | 14.1 | kept for /workspace |
| A18.F21 | Host placement gated on the JuiceFS mount | 13.4 | kept |
| A18.F22 | Live migration relies on no-writeback | 13.10 | kept |
| A19.F0 | Live dashboard: YYC AI operator snapshot poll (live_once) | 21.4 | kept poll; new URL/token |
| A19.F1 | Snapshot fields read by ai_slim (state contract) | 18.3 | field contract kept (golden test, E16) |
| A19.F2 | Snapshot fields read by ai_activity (event diff contract) | 18.3 | kept: paths, `what` values, `owner` email, millisecond `at`; golden test; cursor carried over |
| A19.F4 | Snapshot endpoint auth and caching (producer side the consumer relies on) | 18.1 | separate read-only ops token |
| A19.F6 | Monitor secret JARVIS_OPS_TOKEN and roll annotation | 21.2, 21.4 | `HERMIT_OPS_TOKEN` in yyc/monitor-keys (pair `YYC_TOKEN_MONITOR_HERMIT`) |
| A19.F8 | yyc-mcp reads YYC AI's config.json as the access catalogue | 3.4.1, 21.5 | catalogue.json under the unchanged ConfigMap key `config.json` |
| A19.F12 | CronJob mcp-cleanup (yyc) | 21.5 | unchanged: its catalogue file path and key stay |
| A19.F15 | Daily locked pg_dump of YYC AI's databases | 21.3 | hermit DB added to the dump |
| A19.F16 | YYC AI databases and roles declared in the db namespace | 21.3 | role + Database hermit |
| A19.F17 | Postgres network access for YYC AI pods and sandbox hosts | 21.1 | pg-ingress admits the new host range |
| A19.F18 | Access policies built from config.json users/admins (tofu) | 21.5 | tofu reads catalogue.json in the switch commit |
| A19.F22 | CI: AMI bake follows config.json sandbox.image | 14.3 | AMI follows the pin, pinned aviary |
| A19.F23 | CI: mcp-image smoke test mounts config.json as the catalogue | 21.5 | mount source becomes catalogue.json in the switch commit |
| A19.F25 | Live-dashboard and MCP contract tests in the news stream | 21.4, 21.5 | fixtures follow in the switch commit (`kind: gateway`, new deployments) |
| A20.F1 | Jarvis-mode server tests (fork server in Jarvis mode) | 24.2 | hermit parts (release on delete, warm) ported |
| A20.F3 | Title generation tests | 1.2 | titles are magpie's; no sandbox |
| A20.F4 | Sandbox-preparing indicator tests | 24.2, 4.3 | phases tested in hermit; chip tests magpie's |
| A20.F10 | Local end-to-end harness (no cloud, no model cost) | 24.4 | the platform harness with fake-sandboxd and test-gateway |
| A20.F11 | jarvis3 chat test suite (web, egress, tools, sandbox agent, scaler) | 24.2 | web/vm/packing/ops/common tests ported |
| A20.F12 | chat.yml image smoke test | 17 | smoke ported for the supervisor image, incl. the proxy check |
| A20.F13 | sandboxd Rust unit tests + soak test | 14.6 | kept |
| A20.F14 | Live-migration prototype and benchmark scripts | 24.3 | acceptance incl. FUSE |
| A20.F22 | Measured baselines (acceptance numbers) | 24.3 | baselines are the bar |
| A20.F23 | Zero-downtime rollout configuration verified by tests | 20, 24.2, 24.3 | drain tests + a real rollout check |
| A20.F24 | Other deploy-time checks | 14.3, 16 | AMI check in CI; fresh-host admin route |

---

## 28. Changes this spec needs in the sibling specs

hermit's owner edits only this file. Where a review point also lands in another spec, the change that spec's owner
must make is listed here, so every spec ends up stating the same contract.

**supervisor**
1. `GET /v1/sandbox/self` returns `SandboxSelf` exactly as 3.7.1 (camelCase; `control.tokenSha256` list,
   `claude.managedClaudeMd`, `title.system`, `env` applied to every child, `envPrivate`, `resources` with
   `workspace.quotaInodes`, `lifecycle`, `secrets`, `etag`); the type lives in `hermit-api` and is re-exported by
   `supervisor-api` (supervisor 15.1).
2. `POST /v1/sandbox/activity` with `ActivityReport` every 15 s (`lifecycle.activityEverySeconds`) and on every state
   change, not every 60 s; act on `ActivityReply.release` exactly as on the direct release call (supervisor 15.3).
3. :7070: `GET /v1/health` requires the bearer and answers `{ok, ready, protocol, version, bootId, busy}`;
   `POST /v1/lifecycle/release {reason: ReleaseReason, code, force, deadlineMs}` -> 200 `{released: true}` | 409
   `{code: "sandbox_busy", run}`; the turn it ends is `canceled` for `sandbox_released` and failed for `sandbox_lost`
   (supervisor 15.4-15.5; its own `idle|lifetime|deleted|drain|lost` enum gives way to `ReleaseReason` + `code`).
4. The boot environment of 5.1: the proxy variables carry the credential; no `HERMIT_SANDBOX_TOKEN`; the keeper exits
   non-zero with "no sandbox proxy credential" when they are missing. The VIP example in 17.1 is 10.202.1.1 (R13).
5. Supervisor 16.3: drop "behind the snapshot calls"; its `prepare snapshot` is hermit's flush before a workspace clone
   (3.8), and YYC's snapshot route stays 501 (magpie R28).

**magpie**
1. 8.3 and 8.7 already use hermit's `PUT`, the long-poll and the pull feed (R8); add: a `released` (code
   `sandbox_released`) or `gone` event for a turn still in `admitted`/`preparing`/`starting` whose `start_turn` the
   supervisor has not acknowledged re-acquires a sandbox (`PUT` again) instead of failing the turn; only a release of
   a turn already running fails it.
2. Coverage row 270 (A18.F11 and the other A18 mechanics): "not magpie: hermit keeps the JuiceFS volume, its metadata
   dump and its mounts (hermit 15)", replacing "Postgres backups cover magpie rows".
3. 6.12: the unbounded retry under general unavailability (magpie R26), as the supervisor says.

**platform**
1. 3.10: add the rows magpie 21.3 proposes that touch hermit (R26 retry, R28 snapshot 501 / clone only, R29 link EPERM)
   and these: **R32** the gateway's one addressing mode is the proxy on :3128 (no :8081 listener, no `Host`-routed
   fixed names: 3.1, 7.3, 8.2, 9.7 and 10.2 drop :8081); **R33** sandbox tokens are self-verifying (hermit 6.1) and
   the gateway's hermit dependency is liveness only, cached and stale on error (7.3 and 17.2 item 3 change "token
   lookup by sha256, cached 1 s" to 6.3's steps); **R34** release reasons are hermit's `ReleaseReason` with a `code`,
   shared with the supervisor.
2. 9.2: the `yyc-catalogue` generator is `files: [config.json=catalogue.json]` (the key stays `config.json`), moved in
   the switch commit (18.2 commit B), not in the teardown.
3. 9.6: PV `hermit-jfs` mount options add `all-squash=1000:1000`; hermit's `/jfs` mount has `mountPropagation:
   HostToContainer`.
4. 10.2: `egress-in` admits the sandboxes on 3128 only.

**cutover**
1. P5.4 (line 949) and 9.1's P5 tofu block: the catalogue readers have already moved in P3 (21.5 here); P5.4 only
   deletes `config.json` with `legacy/`, and its "yyc-mcp's catalogue path env follows" goes (nothing about yyc-mcp
   changes, ever). The P3 commit carries 21.5's table and text updates, the tofu `access.tf` path in the same push.
2. P2: run Job `hermit-jfs-owner` before the first hermit import; P5: run it again after `legacy-down.sh` (15.4).
3. P1 items 2-3: `hermit-gateway` and its Sealed Secret live in `yyc-ai-fabric`, not `kube-system` (R24), and no
   `ghcr-pull` Secret is made (R21).

**pigeon**: nothing from hermit's side beyond what it already reads (H1-H4); 4.4's sentence "an empty allowlist is how
a deployment is taken offline" should say the tofu Access policy swap (3.4.1 refuses an empty `users`).

---

## 29. Review round 1: where each point landed

| # | Point (severity) | Landed |
|---|---|---|
| 1 | Catalogue key rename breaks yyc-mcp, mcp-cleanup, the smoke test, docs; timing disagrees (major) | The key stays `config.json`, sourced from catalogue.json; every reader, test and text listed and moved in the switch commit (21.5); P5.4 changes no reader (28) |
| 2 | A18 JuiceFS mechanics unowned (minor) | 15.1-15.6 and coverage rows A18.F0, F5, F11, F13, F16-F19: format kept, meta dump kept on hermit's mount pods (no `--no-bgjob`), ownership uid 1000 with squash options and a fix-up Job, hermit deletes as 1000, mount-pod restart handled; magpie row 270 (28) |
| 3 | Golden test misses `ai_activity` (minor) | 18.3 contract 2 with value rules, switch behaviour, golden test (24.2) and fixture updates (21.4) |
| 4 | magpie and hermit sandbox verbs disagree (blocker) | One contract (3.6): `PUT`, long-poll, `POST …/release {deleted\|offboarded\|requested}`, the pull feed; no 402 or 429 from hermit; magpie 8.3 already matches; re-acquire rule (28) |
| 5 | Workspace API magpie consumes does not exist (blocker) | One model (3.8): share = clone into `uuidv5(SHARE_NS, "share:"+id)`, continue = clone from it, revoke = DELETE, live read = `files`; imports clone too (22.3) |
| 6 | hermit -> supervisor :7070 contract mismatched (blocker) | 3.10: `/v1/health` with bearer and `ready`, `ready: true` marks ready; release `{reason, code, force, deadlineMs}`, 409 honoured; force only for deleted, offboarded, not allowed, operator, recycle, hard lifetime; destroy after 200 or deadline (13.12) |
| 7 | `/self` and activity shapes mismatched (blocker) | One `SandboxSelf`, `ActivityReport`, `ActivityReply` in `hermit-api` (3.7), with `title.system`, `tokenSha256` list, `env` applied by the supervisor (proxy in the boot env, R13), `etag`, and a `release` the supervisor acts on |
| 8 | Supervisor URL and token in the public Sandbox object (major) | Removed (3.6.1); only hermit calls :7070 and only its egress policy allows it (3.10, 21.1) |
| 9 | Two owners stage uploads (major) | The supervisor writes uploads; hermit's write route and staging sentence are gone (3.8) |
| 10 | Person purge has no caller (major) | `POST /v1/admin/people/{email}/purge` -> magpie purge, refused while allowed (16); removal from the catalogue deletes nothing; `GET /conversations/exists` replaced by magpie's `POST /v1/hermit/workspaces/exists` (22.3) |
| 11 | `capabilities` is a behaviour switch (major) | Removed from `/v1/deployment` (3.3); unsupported calls answer 501 for everyone; flags only as a conformance argument (24.1) |
| 12 | Docker socket path and tini (major) | `/run/supervisor/docker.sock`, keeper as PID 1, no tini (5.1, 17) |
| 13 | Gateway owner and routing inconsistent (major) | 6.3 is hermit's half of `gateway-api`; 6.5 names the two YYC parts (egress = HTTP, hermit's fabric in `yyc-ai-fabric` = network) and the one addressing mode (the proxy); `/v1/sandbox/**` routes, `Aviary-Sandbox-*`, 64 MiB bodies, 30 s long-polls, streaming; platform R32 (28) |
| 14 | Pre-reconciliation names (minor) | R1-R25 applied in the text: `/v1/` paths, `Aviary-*` headers, camelCase, `HERMIT_ACCEPT_*`/`HERMIT_TOKEN_*`, namespace `yyc-ai`, `crates/`, `fabric/`, `images/`, `ci.yml`, `aviary-<sha12>`, no CA pin, pool 10 + 2, `person_key` |
| 15 | magpie ops shapes disagree (minor) | 18.4 copies platform 15.3 exactly; `live` is hermit's own count for the monitor (18.3) |
| 16 | Token lookups put hermit on the data path (major) | Signed tokens verified locally (6.1, 6.3); liveness cached 30 s and served stale without limit when hermit fails; never 401/403/5xx because of hermit; lookups served from memory (3.9) |
| 17 | Advisory xact lock across long HTTP calls (major) | Row claims renewed every 5 s, takeover sweep; no transaction across I/O; liveness and people outside the pool (10.1, 10.3) |
| 18 | Migration crash window kills a moved sandbox (major) | Intent recorded first (`migrating_to`), vanished check and orphan sweep respect it, takeover asks the target and adopts, drain hands over (13.10) |
| 19 | Release refusal ignored; admission races (major) | Soft releases honour 409 (13.12); `PUT` sets `last_active_at` and a 120 s reservation that idle and pool respect (3.6.2, 4.2); magpie re-acquires for a not-yet-started turn (28) |
| 20 | Activity cadence and health shapes cause needless releases (major) | 15 s reports, `busy` from `state`, `ready:false` = alive, silence needs two unanswered checks and never runs while the gateway is down; `PUT` never releases (3.6.2, 12) |
| 21 | Single gateway node vs zero downtime (major) | The gap stated, the node fenced off from routine work, hermit judging nothing while it is down, and a maintenance runbook (14.7) |
| 22 | Small contract mismatches (minor) | hermit's parts: `PUT` everywhere, no control token for magpie (3.6.1); origin enum and cancel are magpie's and pigeon's (magpie R30, 8.6) |
| 23 | Retry, link errno, snapshot (minor) | hermit's part: snapshot 501, shares by clone only (3.6.4, 3.8); the platform rows (28) |

### 29.1 Rejected or changed review points

- **Point 20, "in `PUT`, check twice 5 s apart before releasing"**: changed, not adopted as written. `PUT` releases
  nothing at all. In the target design magpie does not call the supervisor, so a `ready` sandbox whose supervisor is
  restarting is still the right one to hand out (the turn waits on magpie's command queue until the supervisor is
  back); a release from `PUT` could only lose a working sandbox. A silent sandbox seen by `PUT` gets the janitor's
  silence check at once, in the background (3.6.2), which uses the two-checks rule.
- **Point 6, "one shared enum" vs point 20, "release reasons as a free string plus code"**: both satisfied by one enum
  (`ReleaseReason`, which machines act on) plus a free `note` (the operator's words, an error class) and the `code`.
  A free-string reason would let two implementations disagree on what `idle` is called.
- **Point 21, "two gateway nodes"**: not built now; the reviewer's alternative (state the gap, runbook, keep the node
  out of routine upgrades) is taken. Two gateways need a Hetzner route swap on failure and a second WireGuard endpoint
  on every host; that is a fabric change of its own and not needed for the switch.
- **Point 16, "MAC the token with a key the gateway also holds"**: changed to an Ed25519 signature with public keys
  published by hermit (`GET /v1/sandbox-token-keys`). The gateway then holds no secret that could mint tokens, needs
  no new sealed value, and learns rotations by itself.

### 29.2 Decided by Deyao

1. A leaver's data (2026-11-02, decision 7): conversations stay after removal from the staff list until an operator
   runs the person purge (16); there is no automatic deletion after any time. Nothing in this spec is open for Deyao
   any more.

---

## 30. The fabric on the new cluster (decision 13)

Text of record: DESIGN 15 (15.5 for the bridges, 15.6 for the gateway). What it changes in this spec:

| Where | Change |
|---|---|
| 13.1 | the gateway is a pod on one Talos node `<gw private IP>`, not a systemd unit on `yyc-pool-small-worker1` (10.0.0.5); the network route points there; the new hosts' JuiceFS metadata is `<gw private IP>:30433`. Ranges, PKI, template, tags and IAM unchanged, so the two fabrics still share nothing (22.2) |
| 13.5 | user data's `juicefs.metaUrl` as above |
| 13.8 | unchanged: hermit pushes peers and egress targets to `https://10.202.0.1:7443`; after a gateway pod restart the next pass (≤ 5 s) re-pushes the full sets, which the adopted data plane already holds |
| 14.1 | the gateway runs no sandboxes and mounts no JuiceFS (unchanged); `setup.sh hub` is gone (the pod is the hub); `setup.sh burst` for AWS hosts unchanged |
| 14.2 | rewritten: sandboxd itself in a host-network privileged pod; data plane kept across process exits and adopted at start; no installer |
| 14.7 | node work only through the runbook, invoked by the Talos upgrade workflow; the node-work hold of DESIGN 15.2 N8; the gap itself unchanged |
| 21.1 | applied by `yyc-ai.yml` on the new cluster; required placement on nodes in network `yyc` (any eu-central location, fsn1 preferred; DESIGN 15.7); the API-server egress by entity `kube-apiserver` (N3); `pg-ingress` (old) gains 10.0.0.0/16 beside its kept 10.244.0.0/16 and 10.201.0.0/24 rules |
| 21.2 | `HERMIT_DATABASE_URL` and `hermit-juicefs`'s `metaurl` name `pg-shared.db`; `HERMIT_JUICEFS_META_URL` names `<gw private IP>:30433`; all sealed with the new cluster's cert (DESIGN 15.9) |
| 22.3 | step 1 after the new cluster's T1-T2; step 5's teardown is on the old cluster's `yyc-pool-apps-worker2`, after which T2 may rebuild that server |
| sandboxd (WP-19) | gateway mode: leave `wg0`, the nft table, ip rules and routes on exit; adopt and reconcile at start from `host.json` and the saved sets; set its interface sysctls itself; acceptance: `kill -9` of the gateway process during a test peer's traffic loses no packet on the data plane and only seconds of :7443; a DaemonSet update with a running sandbox does the same. Only if the new cluster's CNI does not serve NodePorts on traffic arriving on `wg0` (N7): a second VIP port 10.202.1.1:5432 DNATed and masqueraded to the endpoints of `db/pg-shared`, pushed by hermit like the egress targets |

Why this keeps hermit's guarantees: nothing a sandbox depends on moves while it runs. A sandbox on the new fabric
talks to the gateway, the egress VIP and JuiceFS exactly as before; the only new hop (the gateway node forwarding
JuiceFS metadata to the old cluster's `pg-hosts`) is inside network `yyc` and is replaced at T5 by a Service change (selector, target port, the manual EndpointSlice deleted; DESIGN 15.8),
not an address change. Old-fabric sandboxes are untouched until A6.


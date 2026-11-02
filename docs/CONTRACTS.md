# Contracts

For implementers of another hermit or another sandbox gateway. The contract crates are the text of record for the
types, route constants and protocol numbers; this page states the conventions around them.

Status: the conventions, the endpoint names, the gateway contract and the hermit, supervisor and magpie contracts are
complete (`hermit-api`, `supervisor-api`, `gateway-api`, `magpie-api`); sandboxd's is filled in with `sandboxd-api`.

## Conventions (every service)

`crates/aviary-core` implements these once; another implementation of hermit or of the gateway must follow them on
the wire.

### Ports, health, readiness, version

- `8080` serves the API (the Service maps port 80 to it); `9090` serves the ops routes and is never in a Service.
- `GET :9090/healthz` → `200 {"ok": true}`.
- `GET :9090/readyz` → `200 {"ok": true}`, or `503 {"ok": false, "why": [...]}` while the process has not finished
  its own start (each part a named gate, for example `"migrations"`) or while draining (`"draining"`, listed first).
  It never turns 503 because a dependency became unreachable after start.
- `GET :9090/version` → `{"name", "version" (12 hex), "revision" (40 hex), "builtAt" (RFC 3339), "provides":
  {surface: n}, "accepts": {surface: [n-1, n]}}`.
- `GET :9090/metrics` → Prometheus text 0.0.4, names `<svc>_*`, always with `<svc>_build_info{version,revision} 1`
  and `<svc>_draining` (0 or 1).
- `<bin> --version` prints `<name> <sha12> protocols={"provides":…,"accepts":…}`.

### Drain

After the Pod's `preStop` sleep, SIGTERM makes `/readyz` 503, then the service's drain steps run in order, each with
its own deadline: close listeners, end long-polls and streams with their "reconnect" answer, hand back leases and
leader locks, wait for in-flight work. The process then exits 0 (log `drained`). A second SIGTERM exits at once
(code 143).

### Paths and protocol numbers

Every service-to-service route is under `/v1/`; a breaking change adds `/v2/` beside it. Each surface has a protocol
number; clients send `Aviary-Protocol: <surface>=<n>` on every call; a server accepts its current and previous
number.

### Headers

Every header the services define starts with `Aviary-`: `Aviary-Request` (uuid v7 correlation id, minted at the edge
and forwarded unchanged), `Aviary-Protocol`, `Aviary-Person`, `Aviary-Viewer`, `Aviary-View-Session`, `Aviary-Admin`,
`Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation`, `Aviary-Sandbox-Person` (the gateway only), `Aviary-Lease`,
`Aviary-Upload-Name`, `Aviary-Upload-Kind`. A receiver removes every `Aviary-*` header the caller's role may not set
before reading any; a request from outside (a person's app, a sandbox) has all of them removed. Every answer carries
`aviary-request`.

### JSON and the error envelope

`content-type: application/json; charset=utf-8`; camelCase field and query names; unknown fields ignored; times RFC
3339 UTC with milliseconds; ids lowercase UUIDs; emails trimmed and lowercased before any comparison. Every non-2xx
answer is the flat envelope `{"code": "<lower_snake_case>", "message": "<English, for logs>", …fields}`: 400
`bad_request`, 401 `unauthenticated` (with `www-authenticate: Bearer`), 403 `wrong_caller` or a domain refusal, 404
`not_found`, 409 domain conflicts, 410 `cursor_expired`, 413 `too_large`, 415 `unsupported_type`, 429 `rate_limited`
and 503 `unavailable` (with `retry-after`), 507 `quota_exceeded`. Every answer has `cache-control: no-store` unless
the route says otherwise, and `x-content-type-options: nosniff`.

### Caller credentials

A pair token is 32 random bytes, base64url without padding. The caller sends `Authorization: Bearer <token>`. The
callee accepts a comma-separated list per caller (the current token first, two during a rotation) and compares
SHA-256 digests in constant time. Unknown or missing token: 401 `unauthenticated`; a valid token on a route its
caller may not use: 403 `wrong_caller` (the envelope names the `caller`).

### TLS and outbound HTTP

Plain HTTP inside the cluster; outbound HTTPS on rustls with the system trust store (`SSL_CERT_FILE` honoured).
Outbound calls send `User-Agent: <svc>/<sha12>`. Only idempotent calls (`GET`, `HEAD`, `PUT`, `DELETE`, `OPTIONS`, or
a body carrying the caller's idempotency key) are retried, with jittered backoff, on connection errors, timeouts,
429, 502, 503 and 504.

## Endpoint names

A sandbox reaches four fixed names, always through the gateway's proxy (below), never by DNS:

| Name | What answers | Paths a sandbox may use |
|---|---|---|
| `magpie` | the store of record | `/v1/sandbox/**` |
| `hermit` | sandboxes, people and secrets | `/v1/sandbox/**` (`self`, `secrets`, `secrets/{key}`, `activity`) |
| `model` | the deployment's model | `POST /v1/messages`, `POST /v1/messages/count_tokens`, `GET /v1/aviary/models` |
| `tools` | the deployment's MCP tools | `/mcp` |

In the cluster the services are `http://pigeon`, `http://magpie`, `http://hermit`, `http://model` (the gateway's
in-cluster API: the models route for pigeon, `GET /v1/ops/summary` for hermit) and `http://tools`. Inside a sandbox
the supervisor serves hermit on `:7070` and its own processes on the Unix socket `/run/supervisor/agent.sock`.

Protocol surfaces and their numbers (each crate's constants; a server accepts the current and previous number):

| Surface | Number | Crate constant | Spoken by |
|---|---|---|---|
| `hermit-core` | 1 | `hermit_api::CORE` | pigeon, magpie, the gateway's lookups, tools, monitor, admin |
| `hermit-sandbox` | 1 | `hermit_api::SANDBOX` | the supervisor (fixed for a sandbox's life) |
| `supervisor-control` | 1 | `supervisor_api::CONTROL` | hermit, to `:7070` |
| `gateway` | 1 | `gateway_api::GATEWAY` | callers of the models route |
| `magpie-person` | 1 | `magpie_api::PROTOCOL_PERSON` | pigeon; hermit, the ops tools and admin on their routes |
| `magpie-sandbox` | 1 | `magpie_api::PROTOCOL_SANDBOX` | the supervisor (fixed for a sandbox's life) |

## The sandbox gateway contract

Crate `gateway-api` (constants, credential helpers, the revocation view) with the token codec from `hermit-api`;
`testing/test-gateway` is the reference implementation and `aviary-conformance gateway` the check.

**Addressing.** The sandbox's boot environment sets `http_proxy`, `https_proxy`, `HTTP_PROXY`, `HTTPS_PROXY` =
`http://sbx:<sandbox token>@<proxy address>` and `NO_PROXY` = `no_proxy` = `localhost,127.0.0.1,::1`
(`credential::boot_env`). That URL's userinfo is the only place the token travels; the supervisor reads its own token
back from it (`credential::token_from_proxy_url`). Every client sends absolute-form requests (`GET
http://hermit/v1/sandbox/self`) or `CONNECT host:443` to the proxy. A fixed name is an exact host match, ASCII case
insensitive, with no port or port 80 (`FixedName::from_host`). The proxy's own addresses serve the implementation's
local paths (a package mirror, say). Order: credential, fixed name, self host, internet rules.

**Credential.** `Proxy-Authorization: Basic base64("sbx:" + token)` (also `Authorization` on self-host paths).
Missing: 407 with `Proxy-Authenticate: Basic realm="aviary-gateway"`.

**Token.** `hst1.<kid>.<payload>.<sig>`: `kid` decimal; `payload` base64url (no padding) of `version 1 (1 byte) |
sandbox id (16) | conversation id (16) | source IPv4 (4; 0.0.0.0 = any) | expiry (u64 big-endian Unix seconds) |
person (UTF-8, 1-254 bytes)`; `sig` base64url of the Ed25519 signature over `"hermit-sandbox-token\0"` followed by
the binary payload. Verify locally (`KeySet::verify`): it parses, the kid is in the key set, the signature verifies,
`now < expiry`, and the connection's source address (an IPv4-mapped IPv6 address counts as its IPv4) equals the
token's unless that is 0.0.0.0. Any failure is definitive: 403 `{"code": "not_allowed"}`. The key set is `GET
http://hermit/v1/sandbox-token-keys` → `{"keys": [{"kid", "alg": "Ed25519", "publicKey": "<base64url 32 bytes>"}]}`,
refetched every 5 min and at most every 10 s on an unknown kid, the last set kept on a failed fetch.

**Liveness.** `GET http://hermit/v1/sandbox-tokens/{sandboxId}` (the gateway's pair bearer, 1 s timeout) →
`{"sandboxId", "conversationId", "person", "state", "live", "expiresAt"}`; 404 `unknown_sandbox`. Cache 30 s and
refresh in the background; `live: false` or 404 → 403. hermit unreachable, slow or 5xx: keep the last answer whatever
its age; with no answer yet, admit on the verified token. A hermit problem never turns a valid token into 401, 403,
407 or 5xx (`liveness::View`). Revocation takes effect within 30 s.

**Routes.** On a fixed name only the paths of the table above are routed (anything else, and any path with a dot
segment, an empty segment or a percent-encoded `.`, `/` or `\`, is 403 `not_allowed`: `FixedName::allows`). Strip the
client's `Authorization`, `Proxy-Authorization`, `x-api-key` and every `Aviary-*` header; add `Aviary-Sandbox-Id`,
`Aviary-Sandbox-Conversation`, `Aviary-Sandbox-Person` (trimmed, lowercased) from the verified token
(`headers::identity`), `Aviary-Request` (the client's if it is a uuid v7, else new) and the callee's credential: the
gateway→magpie, gateway→hermit or gateway→tools pair bearer, or the model's own credential.

**Transport.** Bodies stream both ways without buffering (SSE, NDJSON); first byte within 200 ms of the upstream's.
Body limit 64 MiB per request on `magpie`, `hermit` and `model` (64 MiB + 1 → 413 `too_large`), tools' own 2 MB on
`tools`. Long-polls held at least 30 s; idle timeout 120 s; the `tools` route's timeout at least 180 s; no total
timeout on `model`. HTTP/1.1 keep-alive both ways. A per-connection upload cap, if the implementation has one, counts
only internet traffic, never fixed names or self-host paths. Added latency on `magpie` and `hermit` p99 ≤ 5 ms.

**Errors.** The gateway's own refusals on fixed names are the JSON envelope: 403 `not_allowed`, 413 `too_large`, 502
`upstream_unreachable`, 503 `unavailable` with `retry-after: 1` (draining). Upstream answers are relayed unchanged.

**Drain.** After the preStop sleep: `/readyz` 503 and no new connections; a new request on a kept-alive connection
is served with `Connection: close`, an idle kept-alive connection is closed; in-flight work finishes up to 600 s for
model streams and CONNECT tunnels, 60 s for `magpie` and `hermit`, 180 s for `tools`; what is left is closed and
counted in `<gw>_drain_cut_total{kind}`.

**Models route.** `GET /v1/aviary/models` → `{"default", "small", "title", "contextTokens", "models": [{"id", "label",
"reasoning"}]}` (`models::Models`), for a sandbox through the proxy and for pigeon in-cluster with its pair token.

## The hermit contract

Crate `hermit-api`: the types (`fixtures/` pins the JSON of each), the route constants (`routes`), the caller table
(`routes::RULES`), the token codec (`token`), a client (`Client`) and a `Server` trait with an axum `router` for it.
Every method of the trait answers 501 `not_supported` by default; an implementation that lacks a call answers exactly
that, and callers treat it as a normal answer. `aviary-conformance hermit` is the check.

| Caller | Routes |
|---|---|
| pigeon | `GET /v1/deployment`, `GET /v1/people/{email}`, `POST /v1/warm`, `GET\|HEAD /ops/*` (with `Aviary-Person`, an admin) |
| magpie | `GET /v1/deployment`, `GET /v1/people`, `GET /v1/people/{email}`, `/v1/conversations/{cid}/sandbox*`, `GET /v1/sandbox-events`, `/v1/workspaces/**` |
| gateway | `GET /v1/sandbox-token-keys`, `GET /v1/sandbox-tokens/{sandboxId}`, the forwarded `/v1/sandbox/**` (with the three `Aviary-Sandbox-*` headers) |
| tools | `GET /v1/sandbox-token-keys`, `GET /v1/sandbox-tokens/{sandboxId}` |
| monitor | `GET\|HEAD /ops/snapshot` |
| admin | `/v1/admin/**` (with `Aviary-Admin`), `GET\|HEAD /ops/*` |

What an implementation must do, beyond the shapes:

- **People.** `GET /v1/people/{email}` answers 200 for any well-formed address (`people::normalise_email`: trimmed,
  lowercased, one `@`, ≤ 254 bytes, no whitespace or control characters; else 400 `bad_email`); an admin is always
  allowed; `allowed == (role != null)`.
- **Sandboxes.** `PUT /v1/conversations/{cid}/sandbox {"person"}` is idempotent per conversation from any number of
  callers: 200 for a ready sandbox, 202 for one starting; 403 `not_allowed`, 409 `person_mismatch`, 503 `sandbox_busy`
  (`{reason: "disabled", retryAfterS}` with `retry-after`). It never runs the start inside the request, never answers
  402 or 429. `GET …/sandbox?waitMs=1..25000&afterVersion=` holds until the version passes `afterVersion` or the
  state leaves `starting`, then answers the current object; 404 `no_sandbox`, except that a sandbox gone less than
  10 min ago is still answered. A new sandbox of a conversation has a new `id`.
- **Release.** `POST …/sandbox/release {"reason": "deleted" | "offboarded" | "requested"}` → `{"released": 0|1,
  "sandbox"}` at once; `deleted` and `offboarded` are forced, `requested` waits for the next idle point. Every end
  carries `release {reason, note, code, message}`: `reason` is one `ReleaseReason` and `code` follows from it
  (`ReleaseReason::code`): `sandbox_released` for an orderly end, `sandbox_lost` when it died, `sandbox_busy` when it
  never became ready for want of capacity, `sandbox_failed` when its start failed otherwise.
- **Pause, resume, snapshot** either work as typed (202, 202, 201) or answer 501 `not_supported`, consistently.
- **Feed.** `GET /v1/sandbox-events?after=<seq>|latest&limit=1..500&waitMs=0..25000` → `{headSeq, events}`: one
  event per visible change, in commit order (seq may have holes), `latest` answers at once; 410 `cursor_expired` for a
  cursor older than the retention. The feed is the only path from hermit to magpie for sandbox state.
- **Workspaces.** Ids are uuids (a conversation's is its id; a share's is `workspaces::share_workspace`). Paths are
  relative, no `.`/`..`/empty components, no NUL, ≤ 4096 bytes and 255 per component (`workspaces::valid_path`), and
  never resolve outside the workspace. `files` answers 200 or 206 for one `Range`, with `etag: "<size>-<mtime ns>"`,
  `last-modified`, `accept-ranges: bytes`; there is no write route; `clone` copies `from` (after asking a ready
  sandbox of it to flush); `DELETE` releases the conversation's live sandbox first and answers 204 also when absent.
- **The sandbox's own routes** answer only with the gateway's credential and three headers matching a live row (else
  404 `no_sandbox`). `GET /v1/sandbox/self` is the descriptor (at most 64 KiB; `etag` is `"` + the first 32 hex of
  SHA-256 over the canonical JSON, keys sorted, of every other field + `"`, also sent as `ETag`; `If-None-Match` →
  304). `GET /v1/sandbox/secrets[/{key}]`: the granted keys; a key not granted is 404 `not_granted`.
  `POST /v1/sandbox/activity` takes the supervisor's report and answers `{state, release}`, where a non-null
  `release` repeats a pending release for the supervisor to act on as on `:7070`'s release call.
- **Boot.** A sandbox starts with the supervisor's keeper as PID 1, the gateway's boot environment (above) and nothing
  else meant for the supervisor; `/workspace` is the conversation's workspace with its quota set; `/home/sandbox/.claude`
  is an empty directory; `/dev/fuse` is available.
- **Control token.** hermit authenticates to `:7070` with `hct_` + base64url(HMAC-SHA256(control key, `"control\0"` +
  the sandbox id as lowercase hyphenated text)); the descriptor carries only its SHA-256 (`control.tokenSha256`, a list
  so a rotation overlaps). Both keys of a token kid come from one 32-byte master key by HKDF-SHA256 with an empty
  salt: info `hermit sandbox token ed25519` gives the Ed25519 seed, `hermit control token` the HMAC key. Signing is
  deterministic, so every replica re-derives the same tokens.
- **Operator page.** `GET|HEAD /ops/` (HTML) and `/ops/snapshot?tz=` (JSON) exist, are read-only and hold no message
  contents; anything else under `/ops/` is 404, other methods 405. Their content is the implementation's.

## The supervisor contract

Crate `supervisor-api` (the supervisor's side of `hermit-sandbox` is re-exported from `hermit-api`).

**`:7070`, surface `supervisor-control`**, for hermit only, with `Authorization: Bearer <control token>` on every
route (401 `unauthenticated` otherwise, 503 `unavailable` with `retry-after: 1` until the descriptor has been read):

| Route | Answer |
|---|---|
| `GET /v1/health` | `{ok, ready, protocol, version, bootId, busy, state}`; `ok && !ready` = alive and starting, never unhealthy |
| `GET /v1/state` | diagnostic run, generation, lease, FUSE, memory and supervisor facts |
| `POST /v1/lifecycle/prepare {kind: pause\|snapshot\|migrate, force, deadlineMs}` | 200 `{ready: true}`; `pause` while busy and not forced: 409 `sandbox_busy` with `run {state, turnId, tasks}` |
| `POST /v1/lifecycle/resumed {kind: pause\|migrate, at}` | 204 |
| `POST /v1/lifecycle/release {reason: ReleaseReason, code, force, deadlineMs}` | 200 `{released: true}` once Claude Code is stopped and every accepted write is stored; not forced while a turn, task or ask is open: 409 `sandbox_busy` with `run`, nothing done |
| `POST /v1/exec {argv, cwd, stdin, timeoutMs, env}` | `{exitCode, signal, stdout, stderr, truncated}` (base64; uid 1000, ≤ 60 s) |

`busy` is true in `starting`, `running`, `stopping` and `held`; false in `idle` and `released`.
`ControlClient::release` and `prepare` read 409 `sandbox_busy` as `Outcome::Busy(run)`.

**The local socket** `/run/supervisor/agent.sock` (HTTP over a Unix socket, no protocol number):
`POST /v1/hooks/send-user-file {toolUseId, caption, files: [{path, size}]}` with the files' descriptors attached as
`SCM_RIGHTS` to the first message (at most 100), 200 `{sent}`; `GET /v1/secrets/{key}` → `{key, value}` or 404
`not_granted`.

## The magpie contract

Crate `magpie-api`: the types (one module per area: `conversations`, `turns`, `events`, `rows`, `live`, `changes`,
`uploads`, `files`, `shares`, `sandbox`, `ops`, `errors`), the route table (`routes::ROUTES`: id, method, path,
callers, success statuses, required headers, surface), a client (`client::Client` with the scopes `person`,
`viewing_as`, `sandbox`, `service`) and a `Server` trait with an axum `router` (`server`). Every method of the trait
answers 501 `not_supported` by default. `fixtures/` pins the JSON of every route (success and error bodies), every
event kind, row kind, change type and command type (`fixtures/README.md`). magpie is part of aviary; the contract is
written down for its other callers (another gateway forwards `/v1/sandbox/**` to it unchanged).

| Caller | Routes | Headers |
|---|---|---|
| pigeon | `/v1/conversations/**`, `/v1/creations/{key}`, `/v1/changes`, `/v1/uploads`, `/v1/shares/**` | `Aviary-Person` (and, viewing as, `Aviary-Viewer`, `Aviary-View-Session`) |
| pigeon | `GET /v1/blobs/{blobId}` (pigeon checked its signed URL), `POST /v1/view-as` | none |
| gateway | `/v1/sandbox/**` | the three `Aviary-Sandbox-*`; the supervisor adds `Aviary-Lease` on every state change and the command poll |
| hermit | `/v1/hermit/**`, `/v1/ops/**` | none |
| admin | `/v1/ops/**`, `/v1/admin/**` | `Aviary-Admin` on `/v1/admin/**` |

What a caller can rely on, beyond the shapes:

- **Idempotency.** M3 by (person, `idempotencyKey`) and M7 by (conversation, `clientMessageId`), for ever: a repeat
  answers 200 with the same body (201/202 the first time; the client returns `Admitted::New` or `Admitted::Repeat`).
  Every cc operation by `opId` (24 h), task events by `id`, asks by `askId`, agent turns by `turnId`, blob PUTs by
  content.
- **Ownership.** Another person's conversation, share or blob answers 404 `not_found`, never 403.
- **Rows.** Every new row and every row update takes the next row seq of the conversation, and nothing else does;
  `epoch` (a decimal string) changes only when a retract removes or rebuilds a visible row, with a
  `timeline_replaced` change. A `turn` change's `timelineSeq` is at or past the turn's last row.
- **Long-polls.** `waitMs` up to 25,000 on M9 (`after` only), M12 and the command poll; the answer comes as soon as
  something is past `after`, else empty at `waitMs`, and at once when the replica drains.
- **Errors as data.** Turn and row error codes are dotted (`sandbox.lost`, `model.budget`, `errors::DataCode`); HTTP
  codes are lower_snake_case (`errors::ErrorCode`, each with its status). magpie returns codes, never sentences.
- **Skew.** An unknown event kind, row kind, change type, command type or code reads as `Unknown`/`Other` and is
  skipped (an unknown command is acked `{"result": {"error": "unsupported"}}`).

## The sandboxd contract

Crate `sandboxd-api`: the types of every route of sandboxd (the host agent of hermit's fabric), the route constants,
a mutual-TLS client (hermit spec 13.15), a `Server` trait with an axum router (what `fake-sandboxd` serves), and the
golden fixtures. sandboxd is not an aviary-core service: HTTPS on the host's overlay address, port 7443, TLS 1.3, a
client certificate with CN `hermit`, no protocol number, and its own error body `{"error": "<message>"}` (also for an
unknown path and for a known path with another method: 404 `not found`).

- **Answers only gain fields (expand-only).** A host keeps the sandboxd of its AMI for hours while hermit rolls
  forward and back, so every client reads the answers of a newer sandboxd: the answer types ignore unknown fields, a
  new answer field is an `Option` or has a serde default, and the enums of answers (`SandboxState`, `ReceiveResult`)
  have an `Unknown` catch-all for a value added later. Removing, renaming or retyping an answer field is a breaking
  change: it waits until no host runs a sandboxd that writes the old shape (a full AMI roll).
- The exact shapes are checked by the tests, not by the types: the fixtures round-trip, sandboxd's own parsers and
  writers are checked against them (`crates/sandboxd/src/contract.rs`), and `tests/live.rs` checks every route's
  answer of a running sandboxd key for key (the soak job).
- Client timeouts and slots: hermit spec 13.15, with the deviations in the crate's `client` module docs (image
  pulls 21 min, a receive's prepare 15 min); one of the 8 per-host slots is kept for the heartbeat.

## Points the crates settle

Where the specs left a detail open or disagreed, the crates decide (platform R35: "the crates' golden fixtures settle
any wording difference"):

- `SandboxSelf` and `ActivityReport` have hermit spec 3.7's shapes (`resources` with `workspace` inside;
  `tasks`, `restarts`, `rssBytes`, `memory {limit, used}` at the top level of the report), not supervisor spec
  15.1/15.3's variants. The report's `state` adds supervisor 15.3's `starting` and `released` to hermit's `running`,
  `stopping`, `held`, `idle`; `busy` is derived (true in `starting`, `running`, `stopping`, `held`), not sent.
- `ActivityEvent.kind` is `memory_kill` (with `pid`, `comm`, `rss`) or `generation_exit` (with `pid`, `exitCode`,
  `signal`); an unknown kind or `ReleaseReason` deserialises as `unknown`, so either enum can grow without breaking
  older readers.
- `:7070`'s `GET /v1/health` includes `state` and the release call's `reason` is `ReleaseReason` (DESIGN 12 D5);
  the `gen` field of `GET /v1/state` is `generation` in Rust.
- The token signature covers the binary payload (not its base64 text); the control token's HMAC input is
  `"control\0"` followed by the sandbox id as lowercase hyphenated text; HKDF uses an empty salt.
- 416 on `files` carries code `bad_range`; the hook route answers `{sent}`; `cpus` is an integer.
- The admin listings (`hosts`, `sandboxes`, `log`, `events`, and the answers of the admin writes other than purge
  and exec) and the ops snapshot are the implementation's own JSON (`serde_json::Value`): the spec names their content,
  not a shape, and no service reads them.
- The exec body types live in `hermit-api` (hermit's audited admin route takes the same body) and `supervisor-api`
  re-exports them.
- The contract crates use `aviary-core`'s error envelope, caller enum and header names, so there is one envelope type
  in the workspace. Their clients read a refusal into their own `Refusal` (status, envelope, `retry-after`).

For magpie (`magpie-api`), where magpie spec 16 and pigeon spec 14.1 (the person surface's single definition,
platform R3) differ, or a detail was open:

- A `Row` carries every row seq it took twice: `seqs` (pigeon 14.1.0) and the same list as `sourceSeqRanges` with
  `updated` (magpie 7.3, the Paseo field). `seqStart`/`seqEnd` are as magpie 7.4 (in an `after` page, `seqEnd` is the
  entry's own seq).
- M6 soft delete answers the Conversation (with `deletedAt` and `purgeAt`, pigeon 14.1), not `{deletedAt}`; M11
  answers 202 `{}` (pigeon), not 200; the share view carries `epoch` (pigeon); `conversation_removed.reason` reads
  `deleted` as well as `purged` (magpie writes `purged`; a soft delete is `conversation_upserted` with `deletedAt`).
- Turn origins are `person`, `agent`, `import` (DESIGN 12 D4), never pigeon 14.1.0's `user`/`cli`. `legacy.cut` is
  not a code (DESIGN 12 D14); `sandbox.lost` is also the supervisor's code for a forced release (supervisor 11.6).
- `ErrorInfo` is `{code, message, status?, detail?, retryAfterS?}` (pigeon's `status` for model errors, magpie 8.3's
  retry time for `sandbox.busy`); an assistant row may carry `error` (the model error tag, pigeon 14.1.18).
- `Ask.toolUseId` is nullable (magpie 7.2) and an ask may carry `title` and `description` (pigeon).
- 409 `conversation_migrating` carries `retryAfterMs`, and `route: "legacy"` in the switch window (platform R37);
  409 `bad_transition` carries the turn's `state` as a type-specific field.
- Routes that answer nothing but must be JSON answer `{}`: M11 (202), M17 (201), `POST /v1/sandbox/asks` (201), agent
  turns (201, 200 for a repeat) and blob PUTs (201, 200 when stored already).
- `GET /v1/ops/view-as?before=` is an RFC 3339 time (platform 15.3; magpie 13 said an id). The admin list is
  `{conversations: [AdminConversation]}`, where an `AdminConversation` is the Conversation plus `person`, `lease`,
  `heartbeat` (with `gen`, `supervisor`, `cc`) and `hermitSandbox` (magpie 13 names these, no shape).
- hermit's, the ops and the admin routes speak `magpie-person` (platform 3.4 names only two magpie surfaces).
- The only snake_case keys are Claude Code's and Anthropic's own: the `data` of `cc.*` events, a tool row's `input`,
  an ask's `input` and `updatedInput`, and the Messages API `response` of a title ack (`tests/shapes.rs`).
- A live `status` frame carries `attempt` and `retryInMs` (supervisor 8.3) beside `status`; `mtimeNs` is a signed
  64-bit integer; the `gen` field of a heartbeat is `generation` in Rust.
- The switch window's routes (tickets, fence, `import_uploads`) are WP-51's and are added to this crate with it.

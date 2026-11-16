# pigeon: the stateless gateway (spec)

Status: design for implementation, 2026-11-02. Authoritative inputs: `/home/claude/artifacts/magpie-target-design.md`
(the target design), Deyao's decisions in the refactor brief, the feature inventory
(`refactor/inventory.json`, every feature whose `target_home` names pigeon is covered in section 21) and gap reports
0-3. Code references are to DE0CH/claudecodeui at 3de3f5d8 (the live `ui` image), DE0CH/jarvis3 at 3f7499a and
DE0CH/YYC at 1775efc4.

Revision 2 (2026-11-02, after review): platform spec 3 and its reconciliation table R1-R25 are applied in the text
(crate paths, ports, `/v1/` routes, `Aviary-*` headers, camelCase bodies, flat lower_snake_case errors, pair-token
names, namespace `yyc-ai`). Section 14.1 is the single contract for magpie's person surface (platform R3 points
here); its row model (14.1.18) is decided here and magpie owns it. Section 22 lists what the other specs must change
to match; section 23 records the review points not taken, with reasons.

pigeon lives in the public repo DE0CH/aviary (crate `crates/pigeon`), is written in Rust, and replaces the `ui` role:
`claudecodeui/jarvis/router.mjs` (the per-person router), the CloudCLI server's HTTP/WS surface, and the Paseo bridge
(`server/modules/paseo/*`). Nothing of CloudCLI runs after the cut.

---

## 1. What pigeon is, and is not

pigeon is the only thing the web and iOS apps talk to. It:

1. verifies the person's Cloudflare Access token (any of the Access teams the deployment names) and asks hermit
   whether that person may use the deployment and in which role;
2. serves the Paseo web build (signed out and signed in, the same files, no per-person anything);
3. speaks the Paseo WebSocket protocol on `/paseo-ws`, translating every app message into magpie (and, for warming,
   hermit) REST calls, and streaming magpie's conversation changes to the open sockets;
4. serves the small REST surface the apps use beside the socket (`/api/...`), each route forwarded to its owner;
5. carries the admin "view as" feature: issues and checks HMAC tokens, gates writes, and makes magpie record the
   audit before anything is sent as the person;
6. forwards the operator page `/ops/*` (admins only, read-only) to hermit.

pigeon holds no durable state and no conversation state. It has no database, no disk writes (read-only root), no
per-person process, no peer-to-peer traffic between replicas, no sticky routing. Any replica serves any request or
socket; killing a replica loses nothing but the open sockets on it, which the apps reopen on another replica and
resume by seq (section 9).

What pigeon never does: run or stop Claude Code, talk to a sandbox or supervisor, read /workspace or JuiceFS, hold
images or uploads beyond streaming them through, know YYC-specific values (product name, Access team, model, allowlist:
all are answers from hermit or `model`), or decide conversation semantics (titles, deletions, turn admission, history
shape: magpie's).

### 1.1 Rules applied

- **Only REST between services.** pigeon calls `magpie`, `hermit` and `model` over HTTP/1.1 (JSON, plus streamed bytes
  for uploads and downloads). Live updates are long-poll GETs (section 8), not a socket or a database listen.
- **Platform conventions** (platform spec 3): every upstream route is under `/v1/` (hermit's `/ops/*` pages excepted,
  platform 3.4); bodies and query parameters camelCase; errors `{"code": "<lower_snake_case>", "message", …}`;
  headers `Authorization: Bearer <pair token>`, `Aviary-Request`, `Aviary-Protocol`, `Aviary-Person`,
  `Aviary-Viewer`, `Aviary-View-Session`, `Aviary-Upload-Name`, `Aviary-Upload-Kind` (platform 3.5); ports 8080 (API)
  and 9090 (ops); drain from `aviary-core` (platform 3.3). pigeon's own public paths are the app protocol and are not
  under these rules (platform 3.4).
- **Fixed endpoint names, no deployment switches.** pigeon reaches `http://magpie`, `http://hermit` and `http://model`
  (k8s Services of those names on port 80 in the deployment's namespace). Its only environment is secrets (section
  3.3). Every value that differs between deployments (Access teams and audiences, product name, locale, iOS URL scheme,
  who may sign in and as what, the model and its label) is read from hermit or `model` at run time.
- **Names.** Code, crate, image, Deployment, Service, Secret, metrics and log fields say `pigeon`; upstream calls say
  `magpie`/`hermit`. No `ui`, `web`, `router`, `jarvis`, `yyc` identifiers in pigeon's own code or manifests. The only
  exceptions are the wire constants of the shipped Paseo clients (1.2), which are not deployment values and not
  component names.
- **Clean cut.** pigeon does not accept the old internal headers (`X-Jarvis-User`, `X-Jarvis-Viewer`,
  `X-Jarvis-View-As`), the `/ws` socket, `/__jarvis/*`, view-as tokens signed with the old derived key, or the
  `access.also` setting. Nothing reads CloudCLI's schemas.
- **Zero downtime** (section 11): at least 2 replicas, surge-first rolling updates, readiness gating, drains that
  hand sockets over by closing them with jitter; turns never depend on a pigeon replica.
- **Read-only product.** pigeon adds no confirmations and no outward actions. It relays an agent's question
  (AskUserQuestion) if magpie reports one (7.5.9), because an unanswerable question is a hang, but whether the agent
  may ask at all is the supervisor's tool policy, not pigeon's.

### 1.2 Wire constants of the shipped clients (`paseo::compat`)

Some strings pigeon must speak contain `jarvis` or `yyc` because the Paseo apps in DE0CH/claudecodeui were written
against today's server. They are wire constants of the client builds listed in the contract suite's
`testing/paseo-contract/clients.txt` (18.1), the same in every deployment, and carry no deployment behaviour. All of
them live in one module, `crates/pigeon/src/paseo/compat.rs`, and nowhere else in pigeon (a CI source rule, platform
4.5, fails a `jarvis`/`yyc` string literal outside that file):

| Constant | Seen by | Where in pigeon |
|---|---|---|
| `/api/jarvis/{signin,app-signin,me,view-as,warm}`, `/api/jarvis/sessions/{id}/{files/raw,shares}`, `/api/jarvis/shares/*` | web and iOS | 5.1 |
| query `jarvis_view_as`, fragment `#jarvis-view-as=` | web (view-as tabs) | 6.1, 6.2 |
| `<meta name="yyc-signed-out" content="1">` | web (signed-out shell) | 5.2.2 |
| message type `yyc.sandbox_status` | web and iOS | 7.6.5 |

Not in this table because they name nothing deployment- or component-specific: `/paseo-ws`, `/api/assets/files`,
`/api/providers/claude/models`, `srv_conversations`, `prj_conversations`, `wks_<id>`. `/yyc-logo.png` is not a pigeon
constant at all: it is a file of YYC's bundle that the Access login page links to, served like any other file.

Rename plan (the same rule as platform 3.4's `/v2` beside `/v1`, not an alias):
1. A claudecodeui change moves the apps to `/api/aviary/*`, `aviary_view_as`, `#aviary-view-as=`,
   `<meta name="aviary-signed-out">` and `aviary.sandbox_status`, and sends `protocolVersion: 2` in hello. Nothing
   else in the protocol changes with it.
2. The same pigeon release serves both protocol 1 (the table above) and protocol 2 (the new names): the socket picks by
   the hello's `protocolVersion`; REST paths are distinct, so both sets are routed; the shell injects the meta of the
   bundle it serves (the bundle and pigeon are pinned together, 17.1). This is two protocol versions side by side,
   each one name per thing.
3. When pigeon's `pigeon_hello_total{protocol="1"}` has been 0 for 30 days (iOS builds are the slow ones; web tabs
   reload within a day), a pigeon release deletes protocol 1 from `compat.rs` and answers it with `hello.rejected
   incompatible_protocol` (7.2), and its build commits leave `clients.txt`.

The rename is not part of the switch (cutover 13.1: no app release for the switch); it is a follow-up task once
pigeon runs.

---

## 2. Position in the system

```
 web app / iOS app
     |  HTTPS + WSS (Cloudflare edge: Access on /api, /paseo-ws, /ops; everything else public)
     v
 cloudflared (tunnel)  ->  Service pigeon:8080  ->  pigeon replicas (>=2)
                                                     |   REST (bearer per pair, person headers)
                       +-----------------------------+------------------+---------------------+
                       v                             v                  v                     v
                 http://magpie                 http://hermit       http://model        https://<team>/cdn-cgi/access/*
     conversations, timeline, changes,   who may sign in, roles,  model list (label)  JWKS, get-identity (first name)
     uploads, blobs, files, shares,      deployment facts, warm,
     view-as audit                       /ops pages
```

A turn: app `send_agent_message_request` -> pigeon `POST magpie /v1/conversations/{id}/turns` -> magpie stores the
user row and starts the turn through hermit/supervisor -> the supervisor stores each transcript write in magpie ->
magpie appends events and, in the same transaction, the timeline rows they make (14.1.18) -> pigeon's long-poll
`GET magpie /v1/conversations/{id}/timeline?after=<seq>&waitMs=25000` returns the new rows -> pigeon pushes
`agent_stream` frames to every socket subscribed to that conversation.

This is the target design's "pigeon streams `GET /conversations/{id}/events?after=<seq>`; a reconnect resumes by seq",
with one refinement the Paseo app forces: the app accepts a live row only when its seq is exactly the previous one
plus 1, so what pigeon streams is magpie's per-conversation sequence of timeline rows (each event that makes or
changes a row takes the next row seq), not the raw event sequence, which also counts events that show nothing
(`turn.started`, `sandbox.*`, `title.changed`, `task.status`, `retract`, `ask.*`). Both sequences are magpie's, kept in
the same transaction; why pigeon cannot derive the row sequence itself is in 23.1.

---

## 3. Process

### 3.1 Crate and dependencies

`crates/pigeon` (binary `pigeon`) in DE0CH/aviary, on `aviary-core` (runtime, drain, ops port, pair tokens, logs)
and the contract crates `magpie-api` and `hermit-api` (typed clients for 14.1 and 14.2, shared with magpie's and
hermit's servers so both sides compile against one set of types). Rust stable per `rust-toolchain.toml`, edition 2024.

| Concern | Crate |
|---|---|
| Runtime | `tokio` (multi-thread) |
| HTTP server, routing, WebSocket upgrade | `axum` 0.8 (`ws` feature, tungstenite underneath), `hyper` 1, `tower-http` (timeouts, request ids) |
| HTTP client | `reqwest` (rustls, HTTP/1.1 pool, `stream` feature) |
| JSON | `serde`, `serde_json` |
| JWT RS256 verify | `jsonwebtoken` (with `rsa` / `ring`), JWKS parsing by hand (`n`,`e`) |
| HMAC, SHA-256, constant-time compare | `ring` (`hmac`, `digest`), `subtle` |
| Multipart | `multer` (streaming) |
| Base64 | `base64` (URL_SAFE_NO_PAD and STANDARD) |
| Caches | `moka` (async, TTL) |
| Metrics | `prometheus` text exposition (hand-rolled registry is fine) |
| Logging | `tracing` + JSON formatter |

Modules: `identity` (Access, people, deployment facts), `static_files` (app bundle, shell), `rest` (the `/api` routes),
`viewas`, `ops`, `paseo` (`conn`, `messages` (types generated and checked as in 18.1), `compat` (1.2), `project`
(mappings), `timeline` (row projection, tool-call mapper port), `strings`), `feeds` (pollers, fan-out), `upstream`
(the `magpie-api`/`hermit-api` clients plus the `model` client, and the migration retry of 7.10), `metrics`.

### 3.2 Ports

| Port | Name | Serves | Exposed |
|---|---|---|---|
| 8080 | `http` | everything public (sections 5-7) | Service `pigeon` port 80 -> 8080; cloudflared only (NetworkPolicy) |
| 9090 | `ops` | `GET /healthz`, `GET /readyz`, `GET /metrics`, `GET /version` (platform 3.2) | not in the Service; kubelet probes and the cluster's metrics scrape only |

`/healthz` and `/readyz` are NOT on 8080 (today they are public on the host and leak the per-replica process count,
gap-1 5.6). On 8080 those paths are ordinary page paths (they get the app shell).

### 3.3 Environment (secrets only)

From Sealed Secret `pigeon` (namespace `yyc-ai`, store `yyc`, section 17.3). No other variables; pigeon refuses to
start if one is missing or empty and logs which (never the value). Names follow platform 3.7 (`<CALLER>_TOKEN_<CALLEE>`).

| Variable | Content | Use |
|---|---|---|
| `PIGEON_TOKEN_MAGPIE` | pair token, 32 random bytes base64url (store `YYC_TOKEN_PIGEON_MAGPIE`) | `Authorization: Bearer` on every magpie call (magpie checks it against `MAGPIE_ACCEPT_PIGEON`) |
| `PIGEON_TOKEN_HERMIT` | same (store `YYC_TOKEN_PIGEON_HERMIT`) | on every hermit call (`HERMIT_ACCEPT_PIGEON`) |
| `PIGEON_TOKEN_MODEL` | same (store `YYC_TOKEN_PIGEON_MODEL`) | on `GET http://model/v1/aviary/models` (`EGRESS_ACCEPT_PIGEON`) |
| `PIGEON_VIEW_AS_KEYS` | comma-separated list of base64url 32-byte keys; the first signs, all verify (store `YYC_PIGEON_VIEW_AS_KEY`, plus `YYC_PIGEON_VIEW_AS_KEY_PREV` during a rotation) | view-as tokens (section 6) |
| `PIGEON_BLOB_KEYS` | same shape (store `YYC_PIGEON_BLOB_KEY`, `YYC_PIGEON_BLOB_KEY_PREV`) | image URL signatures (5.9) |

Pair tokens rotate as platform 3.7 says (the callee accepts two, then the caller moves). pigeon's own HMAC keys rotate
the other way round, since pigeon both signs and verifies: put the new key in `YYC_PIGEON_*_KEY` and the old one in
`_PREV`, reseal, roll; after 12 h (the view-as lifetime; blob URLs: 5.9) drop `_PREV`, reseal, roll (platform 12.2).

Static facts compiled in (identical in every deployment): ports, paths, timeouts and limits in section 13, the string
tables in 7.9, the app bundle path `/srv/app`.

### 3.4 Image

`ghcr.io/de0ch/pigeon:<sha12>` (platform 5.2): static musl binary on `gcr.io/distroless/static-debian12:nonroot`
(uid 65532), read-only root filesystem, built and published by aviary's `ci.yml` (platform 6.1). The app bundle is NOT in this image (17.1): pigeon serves whatever bundle the pod mounts at
`/srv/app`, so app releases and pigeon releases are independent and each deployment pins both.

---

## 4. Identity

### 4.1 Deployment facts from hermit

pigeon reads `GET http://hermit/v1/deployment` (H1, 14.2) at start and every 60 s:

```json
{
  "product": { "name": "YYC AI", "locale": "zh-HK" },
  "signIn": {
    "access": [ { "teamDomain": "yyc-hk.cloudflareaccess.com", "audience": "<staff_ai aud tag>" } ],
    "appScheme": "dev.de0ch.yycai"
  }
}
```

- `signIn.access` is the list of Cloudflare Access teams whose tokens are accepted (A0.F1 "multiple teams"). It is a
  list so a deployment can move between Cloudflare accounts without a pigeon change; YYC has one entry. A change
  takes effect within 60 s, no roll.
- `appScheme` null turns iOS app sign-in off (5.3.3).
- `product.locale`: `zh*` selects the Traditional Chinese (HK) string table, anything else English (7.9).
- Not ready until the first successful read (11.1). A failed refresh keeps the last good copy indefinitely and logs a
  warning every failure (the facts change rarely; an outage of hermit must not take sign-in down).

### 4.2 Token sources

In this order, first non-empty wins (A0.F0, A4.F45):
1. header `Cf-Access-Jwt-Assertion` (set by Cloudflare on Access-covered paths);
2. cookie `CF_Authorization` (URI-decoded);
3. header `cf-access-token` (the iOS app sends it on every REST call, download and the socket,
   `paseo/packages/app/src/yyc/access-token.ts:52`). Today it only works because the app also sends the cookie;
   accepting it removes that accident. Limit 8 KiB; longer is "no token".

### 4.3 Verification

A token is a JWT `header.payload.signature`. Steps (any failure = "not signed in"):
1. `alg` must be `RS256`; `kid` present.
2. Unverified `iss` selects the team: `iss == "https://" + teamDomain` for an entry of `signIn.access`. Unknown issuer
   = refused.
3. Key: the team's JWKS (`https://<teamDomain>/cdn-cgi/access/certs`, `keys[]` with `kid`, `kty=RSA`, `n`, `e`).
   Cache per team 1 h. An unknown `kid` forces a refetch, at most once per 30 s per team. Fetch timeout 5 s. A fetch
   failure keeps the cached set.
4. RSA-SHA256 signature over `header.payload`.
5. `aud` (string or array) contains the team's `audience`.
6. `exp + 5 s >= now` and `nbf - 5 s <= now` (`nbf` optional).
7. `email` claim non-empty; lowercased and trimmed. Service tokens (no `email`, a `common_name` claim) are refused
   ("service tokens are not staff").

Result: `Identity { email, team, exp, claims: { given_name?, name? } }`. Verified tokens are cached by SHA-256 of the
token string for `min(60 s, exp - now)` (moka, 10 000 entries) so the RSA check runs at most once a minute per token.

### 4.4 Who may use the deployment: hermit

`GET http://hermit/v1/people/{email}` (H2, 14.2) answers `{ "email", "allowed": bool, "role": "admin" | "staff" | null }`.
Cache per email per replica 15 s (positive and negative). On a hermit error or timeout (5 s) the last answer is
used for up to 300 s after its expiry (stale-on-error); with no answer at all the request gets 503 (5.12).

Roles: `staff` may use the app; `admin` may also open `/ops/*` and use view-as. `allowed:false` (or role null) = 403.
An empty allowlist (hermit says nobody is allowed) is how a deployment is taken offline: every request and every open
socket is refused within 15 s + one recheck period (7.1.4), whatever token it carries (fixes gap-1 3.1, where a
policy swap at Access left 720 h sessions working).

The `@domain` rule, the explicit list, and its sharing with the Access policies and yyc-mcp are hermit's
implementation (A0.F2, A7.F5); pigeon never sees the list.

### 4.5 First name for the greeting

`firstName(identity)`: `given_name`, else the first whitespace-separated word of `name` (a name with CJK characters is
kept whole), at most 40 characters, null if it contains `@` or is empty; never derived from the address (A0.F9).
If the token carries neither claim: `GET https://<teamDomain>/cdn-cgi/access/get-identity` with
`Cookie: CF_Authorization=<token>`, 1.5 s timeout, reading `given_name` / `name` / `oidc_fields.given_name` /
`oidc_fields.name`. Cache per email per replica: 6 h, or 5 min after a failure.

### 4.6 The request identity

Every request on 8080 that is not a static asset resolves to one of:

| State | Meaning |
|---|---|
| `SignedOut` | no token, or the token failed 4.3 |
| `NotAllowed(email)` | verified, hermit says not allowed |
| `Person { email, role, exp }` | verified and allowed |
| `ViewAs { admin, person, session, exp }` | a `Person` with role admin carrying a valid view-as token (6.2) |
| `ViewAsRefused` | a view-as token was present but not honoured (6.2): never falls back to the caller's own data |
| `Unavailable` | hermit unreachable and nothing cached |

---

## 5. HTTP surface (port 8080)

### 5.1 Routing table

Order matters; first match wins. "Signed-in prefixes" are `/api`, `/paseo-ws`, `/ops` (exactly these three; `/ws`
and `/__jarvis` leave the Access app in the same change, 17.5).

| # | Method | Path | Identity needed | Handler |
|---|---|---|---|---|
| 1 | GET/HEAD | `/api`, `/api/`, `/paseo-ws` (no upgrade) | none | 302 `/` (Access returns a sign-in started on its own login page to `/api`, A0.F8) |
| 2 | GET | `/paseo-ws` with `Upgrade: websocket` | Person or ViewAs | section 7 |
| 3 | GET | `/api/jarvis/signin?next=` | Person | 5.3.1 |
| 4 | GET | `/api/jarvis/app-signin?state=` | Person | 5.3.3 |
| 5 | GET | `/api/jarvis/me` | Person or ViewAs | 5.4 |
| 6 | POST | `/api/jarvis/view-as` | admin Person | 6.1 |
| 7 | POST | `/api/jarvis/warm` | Person or ViewAs | 5.6 |
| 8 | GET | `/api/providers/claude/models` | Person or ViewAs | 5.5 |
| 9 | POST | `/api/assets/files` | Person or ViewAs | 5.7 |
| 10 | GET/HEAD | `/api/jarvis/sessions/{id}/files/raw` | Person or ViewAs | 5.8 |
| 11 | GET | `/api/blobs/{blobId}` | Person or ViewAs (+ signature) | 5.9 |
| 12 | GET, POST | `/api/jarvis/sessions/{id}/shares` | Person or ViewAs (POST: Person) | 5.10 |
| 13 | DELETE | `/api/jarvis/shares/{shareId}` | Person | 5.10 |
| 14 | GET | `/api/jarvis/shares/{shareId}` | Person or ViewAs | 5.10 |
| 15 | GET/HEAD | `/api/jarvis/shares/{shareId}/files/raw` | Person or ViewAs | 5.10 |
| 16 | POST | `/api/jarvis/shares/{shareId}/continue` | Person | 5.10 |
| 17 | any | other `/api/*` | (identity first) | 404 `NOT_AVAILABLE` (5.12) |
| 18 | GET/HEAD | `/ops`, `/ops/*` | admin Person | 5.11 |
| 19 | GET/HEAD | path with an extension, not under a signed-in prefix | none | static file (5.2.1) |
| 20 | GET/HEAD | any other path not under a signed-in prefix | optional | app shell (5.2.2) |
| 21 | other methods on 19/20 | | | 405 |
| 22 | any | `Upgrade: websocket` on any path but `/paseo-ws` | | 404 (raw HTTP), no upgrade |

Identity failures on rows 2-18: `SignedOut` -> 401, `NotAllowed` -> 403, `ViewAsRefused` -> 403
`VIEW_AS_REFUSED`, `Unavailable` -> 503. Body: JSON `{"success":false,"error":{"code","message"}}` on `/api/*` (the
apps' frozen error shape, which platform 3.4 keeps outside the service-to-service conventions of platform 3.6)
(code `NOT_SIGNED_IN`, `NOT_ALLOWED`, `VIEW_AS_REFUSED`, `UNAVAILABLE`); the HTML page (5.2.3) on `/ops`; a raw
HTTP response on the upgrade (`HTTP/1.1 401 Unauthorized`, text body). The iOS app treats any non-2xx or non-JSON
`/api/jarvis/me` as signed out (`yyc/access-gate.tsx:22-33`), so JSON 401 is compatible.

In view-as, rows 6, 13, 16 and `POST` on row 12 answer 403 `VIEW_AS_READ_ONLY` before any upstream call (6.3).

URL handling: the path is percent-decoded once and normalised (`.`/`..` segments resolved) before matching; a path
containing NUL, a backslash, or that changes prefix class after normalisation (for example `/ops/../api/x`) is 404.
The query parameter `jarvis_view_as` is consumed by pigeon and never forwarded.

### 5.2 Static files and the app shell

The app bundle is mounted read-only at `/srv/app` (17.1): `index.html`, `_expo/static/**` (hashed chunks of the
current and recent builds), `sw.js`, `manifest.json`, icons, `yyc-logo.png`, fonts. pigeon loads the file list into
memory at start (paths, sizes, precompressed `.br`/`.gz` siblings) and reads bodies from disk with the OS page cache.

#### 5.2.1 Files (path with an extension)

- Served to anyone, signed in or not, with no identity check and no upstream call (fixes A15.F2: a chunk fetch never
  touches a per-person anything). The only paths that need a token are the signed-in prefixes, which never match here.
- Lookup is in the in-memory list only (no filesystem path joining, so no traversal).
- Headers: `content-type` from the MIME map below; `x-content-type-options: nosniff`; `referrer-policy: same-origin`;
  `content-length`; `etag` (the build's content hash for hashed names, else size+mtime); `vary: accept-encoding` when a
  precompressed sibling exists, and `content-encoding: br` (or `gzip`) when the client accepts it.
- `cache-control`: `.html` -> `no-store`; a hashed name (under `/_expo/static/`, or matching `[.-][0-9a-f]{16,}\.[a-z0-9]+$`)
  -> `public, max-age=31536000, immutable`; `/sw.js` -> `no-cache` (browsers must see a kill-switch update, A15.F10);
  anything else -> `public, max-age=300`.
- Missing file -> 404 `text/plain` `Not found`, `cache-control: no-store` (never the shell and never a cacheable 404,
  so the edge cannot pin a 404 for a chunk that a newer replica has, A15.F3).
- MIME map (from `router.mjs:113-119`): html, js/mjs (`text/javascript; charset=utf-8`), css, json, map,
  webmanifest (`application/manifest+json`), png, jpg/jpeg, gif, svg, ico, webp, woff, woff2, ttf, otf, txt, wasm;
  other -> `application/octet-stream`.
- `/yyc-logo.png` is an ordinary public file: the Access login page uses it as its logo (`tofu/access.tf:29`).
- `/sw.js` is the kill-switch service worker that unregisters the pre-Paseo CloudCLI worker (`claudecodeui
  public/sw.js`). The app bundle carries it (17.1); serving it signed out too fixes A15.F10 (today a signed-out update
  check gets 401 and the old worker survives). Retire it once Cloudflare analytics show no `/sw.js` requests for 30
  days (a later release removes the file from the bundle).

#### 5.2.2 The shell (page paths)

Any other GET/HEAD outside the signed-in prefixes gets `index.html`. This is the SPA fallback every app route relies
on, signed in or not (A4.F50): `/`, `/new`, `/h/<host>`, `/h/<host>/workspace/wks_<id>`, `/h/<host>/agent/<id>`,
`/s/<shareId>`, `/sessions`, Settings and Recently deleted paths, and any unknown page path (the app's router shows its
own not-found). A query string or fragment does not change the answer. The deep links of the 301 from
`ai.yyc.deyaochen.com` land here path-for-path (A19.F20).

pigeon changes `index.html` only by the two substitutions below and serves every other byte as built. In particular
the static page frame and its early message box (a plain `<textarea>` over the composer's place, typable before any
JavaScript loads, A4.F47, A4.F48) are part of the built `index.html` (`scripts/build-paseo-ui.mjs`), and must reach
the browser unchanged: no script injection, no reordering of `<head>`, no CSP that blocks its inline script. The
signed-out meta is what makes the app skip the early box when signed out (A4.F47), so it goes in before `</head>`,
ahead of any script. The rewritten shell is:
- `<title>` and `<meta name="apple-mobile-web-app-title" content="...">` set to the HTML-escaped `product.name`
  (A1.F46, A4.F53);
- when the identity is not `Person`/`ViewAs`: the signed-out meta of 1.2 (`<meta name="yyc-signed-out"
  content="1">` for protocol-1 bundles) injected before `</head>` (A0.F5, A4.F43). The app then shows the disabled new-chat layout with 「登入」, opens no socket and makes no API call.
- Identity on page paths is checked only from the cookie (the edge does not send the assertion header there). A
  verified but `NotAllowed` cookie gets the 403 page (5.2.3) instead of the shell; `Unavailable` gets the signed-out
  shell (the app then retries through `/api/jarvis/me`).
- Headers: `content-type: text/html; charset=utf-8`, `cache-control: no-store`, `vary: cookie`, nosniff,
  `referrer-policy: same-origin`.
- The two rewritten variants (signed in, signed out) are rendered once per (bundle, product name) and kept in memory.

#### 5.2.3 HTML pages pigeon writes itself

All `cache-control: no-store`, `lang` from the locale, the same minimal markup as `router.mjs:384` (`page()`):

| Status | zh title / text | en title / text |
|---|---|---|
| 401 | 未登入 / 請重新整理頁面登入。 | Not signed in / Reload the page to sign in. |
| 403 | 沒有權限 / {email} 沒有使用 {product} 的權限。 | Not allowed / This account ({email}) may not use {product}. |
| 403 ops | 只限管理員 / 只有管理員可以開啟這個頁面。 | Admins only / Only an admin can open this page. |
| 405 | 唯讀 / 這個頁面只可閱讀。 | Read-only / This page is read-only. |
| 502 ops | 暫時未能使用 / 控制台沒有回應，請稍後再試。 | Unavailable / The operator page did not answer. Try again shortly. |
| 503 | 暫時未能使用 / 系統繁忙，請稍後再試。 | Unavailable / The service is busy. Try again shortly. (`retry-after: 5`) |

Both languages are shown on one page (zh first) as today, so a page is right whatever the browser.

### 5.3 Sign-in

#### 5.3.1 `GET /api/jarvis/signin?next=<path>` (web 「登入」)

Behind Access, so the person has just signed in. 302 to `next` if it matches `^/(?![/\\])\S*$` and its path part is
not under a signed-in prefix; else 302 `/`. Headers `cache-control: no-store`, `referrer-policy: no-referrer`. A
`SignedOut`/`NotAllowed` caller gets the 401/403 as for any `/api` path (A0.F7).

#### 5.3.2 Sign out

Web: the app links to `/cdn-cgi/access/logout`, answered by the Cloudflare edge; pigeon is not involved. iOS: the app
drops its Keychain token (A4.F44). Nothing to build.

#### 5.3.3 `GET /api/jarvis/app-signin?state=<state>` (iOS)

- `appScheme` null -> 404 page. Method other than GET -> 405. `state` must match `^[A-Za-z0-9_-]{16,128}$` -> else 400.
- The person must be `Person` (a view-as token here is ignored: the app's ASWebAuthenticationSession never carries
  one).
- 302 `Location: <appScheme>://signed-in?state=<state>#token=<percent-encoded token>`, where the token is the
  `CF_Authorization` cookie Access just set, else the assertion header. `cache-control: no-store`,
  `referrer-policy: no-referrer`. The token is in the fragment, never in a log or a Referer. pigeon logs only
  `app-signin` + userKey.
- The app stores the token in the Keychain (`yyc.access-token`) and sends it as `cf-access-token` + `Cookie` for
  720 h (Access `session_duration`, A0.F10, A7.F17).

### 5.4 `GET /api/jarvis/me`

```json
{ "firstName": "Deyao", "sharing": true }
```
View-as: `{ "firstName": <the person's cached first name on this replica or null>, "sharing": true,
"viewAs": { "user": "<person>", "expiresAt": "<ISO>" } }`. `cache-control: no-store`. No upstream call except a
possible get-identity (4.5). The app loads it once with a 3 s timeout for the greeting and as the "signed in?" probe
(A4.F42, A4.F45). `sharing` is always true (the share menu is a product feature everywhere; it was false only in the
retired local mode).

### 5.5 `GET /api/providers/claude/models`

From `GET http://model/v1/aviary/models` (14.3, platform R18), cached 60 s per replica; the deployment's default model
is the answer's `default`.

```json
{ "success": true, "data": { "models": {
  "OPTIONS": [ { "value": "deepseek/deepseek-v4.1-flash-1017", "label": "DeepSeek V4.1 Flash" } ],
  "DEFAULT": "deepseek/deepseek-v4.1-flash-1017" } } }
```
One option per entry of `models`: `value` = `id`, `label` = `label` (fallback `id`); `DEFAULT` = `default`. On a
`model` error with nothing cached: 502
`{"success":false,"error":{"code":"MODEL_LIST_UNAVAILABLE"}}`; the app retries (`yyc/model-label.tsx`). The same
cached value feeds the providers snapshot on the socket (7.5.1). (A4.F28, A10.F18, A2.F12.)

### 5.6 `POST /api/jarvis/warm`

Body `{"sessionId"?: "<uuid>"}` (≤ 4 KiB, JSON). Always answers 202 `{"ok":true}` at once (A1.F45, A0.F34). Then, at
most once per (person, conversation-or-new-chat) every 30 s per replica (in-memory map, 1 000 keys, oldest evicted),
fire-and-forget `POST http://hermit/v1/warm {"person": email, "conversationId": id|null}` (H3) with a 10 s timeout; failures
are logged at debug only. With N replicas hermit may see up to N hints per 30 s: harmless, it is a demand hint.
Allowed in view-as (the hint is for the person); the app does not send it from a read-only tab anyway.

### 5.7 `POST /api/assets/files` (upload for a message)

Request: `multipart/form-data`, one part named `files` with a filename (the app sends one file per request,
`composer/actions.ts:131-158`). Response 201 (the app accepts any 2xx):
```json
{ "attachments": [ { "path": "uploads/7c9e.../report.xlsx", "name": "report.xlsx", "size": 123456,
                     "mimeType": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" } ] }
```
Steps:
1. CSRF check (5.13). In view-as it is allowed (the admin's override send needs it, A0.F14).
2. `Content-Length` (if given) > 53 477 376 (50 MiB + 1 MiB multipart overhead) -> 413 at once.
3. Stream-parse with `multer`; exactly one `files` part (none or more than one -> 400
   `{"error":"..."}`). The part's bytes stream straight to M13 `POST http://magpie/v1/uploads` with
   `Content-Type` = the part's type (or from the extension table when absent/`application/octet-stream`),
   `Aviary-Upload-Name` = the percent-encoded filename (≤ 255 bytes after sanitising: no `/`, `\`, NUL, control
   characters), `Aviary-Upload-Kind: file`. pigeon buffers at most 64 KiB at a time (backpressure to the client).
   An upload that has started is finished even if the replica begins draining meanwhile (11.2 step 4).
4. More than 52 428 800 bytes of file content -> abort both sides, 413.
5. magpie's 201 `{uploadId, path, name, size, mimeType}` -> pigeon answers `{attachments:[{path,name,size,mimeType}]}`.
   magpie's 413 -> 413; 415 -> 415; anything else -> 502.
- Error bodies are `{"error": "<localized sentence>"}` because the app shows `body.error` as is
  (`composer/actions.ts:142-147`): 413 -> zh `「{name}」太大（上限 50MB）。` / en `"{name}" is too large (max 50MB).`;
  415 -> zh `不支援這種檔案。` / en `This kind of file is not supported.`; 502 -> zh `上載失敗，請再試一次。` /
  en `Upload failed. Please try again.`
- The cap is one number everywhere: 50 MiB, the app's `FILE_UPLOAD_MAX_BYTES`. magpie enforces it as the owner of
  the blob store (gap-0 9 and 14.4); pigeon's check is only there so it never streams more than that.
- `path` is what magpie returns; the app passes it back as the attachment's `id`/`path` on the next send (7.5.4) and
  shows `name`. Uploads are person-scoped (a new chat has no conversation yet); magpie collects unreferenced ones.

### 5.8 `GET /api/jarvis/sessions/{id}/files/raw?path=<rel>&inline=1&sent=<toolUseId>`

Sent-file cards and paths named in replies (A2.F39, A4.F36, A4.F37). pigeon validates `id` (UUID), `path` (≤ 4096
characters, no NUL, relative), `sent` (`^[A-Za-z0-9_-]{1,128}$`) and `inline` (`1` or absent), then streams
M14 `GET http://magpie/v1/conversations/{id}/files/raw?path=&sent=&inline=` to the client. Passed through from
magpie: status, `content-type`, `content-length`, `content-disposition`, `last-modified`, `etag`, `accept-ranges`,
`content-range`, `content-security-policy`, `x-content-type-options`, `cache-control`. The client's `Range` and
`If-None-Match`/`If-Modified-Since` go up. magpie's JSON errors (400/404) pass through. No body is buffered. View-as
reads the person's files (GET is allowed).

### 5.9 `GET /api/blobs/{blobId}?v=<variant>&sig=<sig>` (images in the timeline)

Images in user messages are blobs in magpie's bucket (target design: "row = reference + thumbnail/preview; full
content fetched on tap"). The app renders `user_message.images[].url` directly with `fetch(url, {credentials:
'include'})` and an `<Image>` source, without `?jarvis_view_as=` and, on iOS, without the Access headers in some
paths (`attachments/service.ts:136`). A per-request owner check would therefore break images in view-as tabs and
shares. So image URLs are capabilities signed by pigeon:

- `variant` in `thumb` (long edge 512 px), `display` (long edge ≤ 2000 px, the model copy) or `full` (original).
- `sig` = base64url(HMAC-SHA256(first key of `PIGEON_BLOB_KEYS`, `blobId + "|" + variant`))[..22] (128 bits).
  Verified against every key in the list, constant time.
- The URL is still under `/api` (Access + pigeon sign-in required); the signature is the authorisation to that one
  blob, like a share id. Deleting or purging a conversation deletes its blobs in magpie, so the URL dies with it.
- pigeon streams M15 `GET http://magpie/v1/blobs/{blobId}?variant=` with headers as 5.8;
  `cache-control: private, max-age=86400, immutable` (blob content never changes). Bad signature -> 404.
- Rotation of `PIGEON_BLOB_KEYS` invalidates URLs held in the apps' replica caches; they refetch the timeline after
  a 404 only when the conversation is reopened. Rotate only when a key leaks.

### 5.10 Shares

The app's share sheet and the read-only share page `/s/<id>` (A1.F32, A4.F40, A4.F41). All forwarded to magpie with
the person headers; shapes to the app are unchanged:

| App call | magpie call (M16) | Answer to the app |
|---|---|---|
| `GET /api/jarvis/sessions/{id}/shares` | `GET /v1/conversations/{id}/shares` | 200 `{"shares":[{"id","url":"/s/<id>","createdAt"}]}` |
| `POST /api/jarvis/sessions/{id}/shares` | `POST /v1/conversations/{id}/shares` | 201 `{"id","url":"/s/<id>","createdAt"}`; magpie 409 `nothing_to_share` -> 409 `{"success":false,"error":{"code":"NOTHING_TO_SHARE","message":...}}` |
| `DELETE /api/jarvis/shares/{shareId}` | `DELETE /v1/shares/{shareId}` | 204 |
| `GET /api/jarvis/shares/{shareId}` | `GET /v1/shares/{shareId}` | 200 `{"id","title","ownerName","isOwner","createdAt","timeline":[PaseoRow...]}`; 404 -> 404 |
| `GET /api/jarvis/shares/{shareId}/files/raw?path&sent&inline` | `GET /v1/shares/{shareId}/files/raw?...` | bytes as 5.8 |
| `POST /api/jarvis/shares/{shareId}/continue` | `POST /v1/shares/{shareId}/continue` | 201 `{"sessionId":"<new conversation id>"}` |

`timeline` rows are projected with the same function as the live timeline (7.8) from the rows magpie returns inline
(`rows`, the shared conversation's timeline rows up to the share point, 14.1.18), with image URLs signed (5.9). pigeon
streams the projection out row by row as it parses magpie's answer, so a long shared conversation is never held twice
in memory. `url` is built by pigeon as `/s/<shareId>` (relative; the app makes it absolute). Error code mapping
magpie -> app: `not_found` 404 `SHARE_NOT_FOUND`, `nothing_to_share` 409 `NOTHING_TO_SHARE`,
`conversation_migrating` after the retry of 7.10 -> 409 `CONVERSATION_MIGRATING` with the 7.10 sentence as
`message`, else 502 `SHARE_FAILED`. The audience is every person hermit allows (the 128-bit share id is the
capability).

### 5.11 `/ops/*` (operator page)

1. `/ops`, `/ops/...`, `/ops?...` only; resolved after identity. Not admin -> 403 ops page, nothing forwarded, a warn
   log with userKey.
2. Only GET/HEAD (else 405 page `Read-only`).
3. After normalisation the path must still be `/ops` or under `/ops/` (else 404). `/ops` exactly -> 302 `/ops/`.
4. Forward to `http://hermit/ops<rest-of-path><query>` (H4; not under `/v1`, platform 3.4) with
   `Authorization: Bearer $PIGEON_TOKEN_HERMIT`, `Aviary-Person: <admin email>`, `Aviary-Request`, the client's
   `Accept` and `Accept-Encoding`. No other client header and no cookie
   goes up. Timeout 30 s.
5. Response passthrough: status, `content-type`, `cache-control`, `content-security-policy`, `content-encoding`,
   `x-content-type-options`, body streamed. Hop-by-hop headers and `set-cookie` are dropped.
6. hermit error or timeout -> 502 ops page.

hermit (YYC implementation) serves the page and its snapshot JSON (`/ops/snapshot?tz=`) and aggregates from magpie
and egress over REST (gap-2 5.1). Its 「View as」 form posts to `/api/jarvis/view-as` (6.1), same origin. A view-as
tab cannot open `/ops` (rows 18: a ViewAs identity is refused like a non-admin, because the identity is the person).
pigeon re-checks admin per request; hermit re-checks the bearer and `Aviary-Person` role itself (A0.F11, A19.F4).
The news-stream dashboard's snapshot feed reaches hermit inside the cluster with its own token; it does not go
through pigeon.

### 5.12 Everything else under `/api`

404 `{"success":false,"error":{"code":"NOT_AVAILABLE","message":"This feature is not available here."}}`,
`cache-control: no-store`, after identity (so a signed-out probe still gets 401). This is an allow-list: every
CloudCLI route not in 5.1 is gone (section 15). Never the shell for an `/api` path (A10.F23).

### 5.13 Same-origin, CORS, limits

- No CORS. pigeon sends no `access-control-*` headers; `OPTIONS` on `/api/*` -> 405 (Access already answers browser
  preflights with 403, gap-1 4).
- **Developer local mode (A4.F58) is dropped.** `EXPO_PUBLIC_LOCAL_DAEMON` pointed a Metro dev page on another
  origin at a dev server; against pigeon that is cross-origin and refused by design, and today it already could not
  use the YYC REST features. The development path is same-origin instead: `npm run build:web` (or `expo export`
  watching) into a directory that a local pigeon from the aviary harness serves as `/srv/app` (platform HARNESS.md,
  with fake-access and fake magpie/hermit), so the page, `/api` and `/paseo-ws` share one origin as in production.
  The `EXPO_PUBLIC_LOCAL_DAEMON` branch of `paseo/packages/app/src/yyc/api.ts` is deleted in the claudecodeui change
  that adds `pigeon-app.yml` (17.1).
- Every response on 8080 carries `server: pigeon/<sha12>` (useful in incidents; it names the build, nothing else).
  Through Cloudflare the edge replaces `server` with its own, so the switch runner's probe (cutover 17.2 item 3)
  tells pigeon by the `aviary-request` header every response also carries, which the edge passes.
- State-changing requests (POST/DELETE on `/api/*`) must be same-origin: `Sec-Fetch-Site` is `same-origin` or `none`,
  or, without that header, `Origin` is absent (native iOS) or its host equals `Host`. Otherwise 403
  `{"code":"CROSS_ORIGIN"}`. This replaces CloudCLI's permissive `cors()` (A10.F28).
- Body limits per route (bytes): view-as form 4 096; warm 4 096; shares POST/continue 4 096 (empty bodies expected);
  upload 53 477 376 (5.7). Anything else with a body on a GET -> ignored and drained up to 64 KiB, then the connection
  is closed.
- HTTP server timeouts: request headers 30 s; idle keep-alive 120 s (above cloudflared's 90 s origin idle, so pigeon
  never closes a connection cloudflared is about to reuse); no total request timeout (uploads and downloads stream).
  Every upstream call has its own timeout (section 13).
- Cloudflare's own limits apply in front: 100 MB request body, 100 s to first response byte (524). Every pigeon REST
  route answers its headers well inside 100 s.

---

## 6. View as

An admin opens the app as a staff member to see what they see, read-only by default, with an override that lets the
admin send as the person (gap-2 2.11). The client side is unchanged (`yyc/view-as.ts`, `view-as-box.tsx`).

### 6.1 Start: `POST /api/jarvis/view-as`

1. Caller must be an admin `Person` (not ViewAs) -> else 403 page, warn log `view-as start refused`.
2. POST only (405 page otherwise). Same-origin: `Sec-Fetch-Site: same-origin`, or without it `Origin` host == `Host`
   (403 page otherwise; `none` is NOT accepted here: the start must come from the ops page form).
3. Body: `application/x-www-form-urlencoded`, ≤ 4 096 bytes, field `user`, trimmed and lowercased. Must not start
   with `@`; hermit `GET /v1/people/{user}` must say `allowed` (400 page `That address is not a user of this
   instance.` otherwise; former users are therefore refused, A17.F16).
4. Claims `{ "a": admin, "u": user, "s": <random UUIDv4>, "e": now + 43 200 }` (12 h, seconds).
5. Record first: M17 `POST http://magpie/v1/view-as {"admin","person","session","expiresAt"}`, 5 s timeout. Not
   201 -> 502 page `The view could not be recorded, so it was not opened. Try again in a moment.` and no token.
6. 303 `Location: /#jarvis-view-as=<token>` (the fragment of 1.2), `cache-control: no-store`, `referrer-policy: no-referrer`. Info log
   `view-as started` with both userKeys and the session.

### 6.2 Token

Format (unchanged, the client parses the claims, `view-as.ts:28-41`):
`base64url(JSON{a,u,s,e}) + "." + base64url(HMAC-SHA256(key, first part))`, no padding.

Checks, in order, on every request that carries `?jarvis_view_as=` (REST, file links, the socket URL):
- ≤ 2 048 characters, exactly two parts, the MAC matches one of `PIGEON_VIEW_AS_KEYS` (constant-time), claims have the
  right types (`a`,`u`,`s` strings, `s` ≤ 64 characters, `e` integer), `e > now`;
- the caller's own verified Access email is an admin now (hermit, live) and equals `a`;
- `u` is still allowed (hermit).

Pass -> identity `ViewAs{admin:a, person:u, session:s, exp:e}`: everything is served as `u`. Fail -> `ViewAsRefused`
(403 `VIEW_AS_REFUSED` "This view-as link is not valid for this account, or has expired."), never the caller's own
data (A0.F13). Tokens are stateless; any replica checks any token. Tokens signed by the old router
(`sha256('jarvis-view-as|'+JARVIS_CC_TOKEN)`) are accepted while any conversation is legacy: their MAC is checked
against the derived legacy key `PIGEON_LEGACY_VIEW_AS_KEY` with the same claim checks, and pigeon re-signs its LU
calls for that view with it, so an admin's view-as tab keeps working across the move (DESIGN 9.5, decision 4). The
legacy key and its check are deleted in A6 (old tokens live at most 12 h).

### 6.3 Read-only enforcement, HTTP

While `ViewAs`: GET/HEAD on any allowed route; POST only `/api/jarvis/warm` and `/api/assets/files`; everything else
-> 403 `{"success":false,"error":{"code":"VIEW_AS_READ_ONLY","message":"Read-only while viewing as someone else."}}`
before any upstream call (A0.F14). `/ops` is refused (5.11). The socket is allowed (it is the only socket).

### 6.4 Read-only enforcement, socket

A ViewAs socket carries `viewer = {admin, session}`. pigeon:
- refuses with `rpc_error {requestId, requestType, error: "Read-only while viewing as someone else.", code:
  "view_as_read_only"}` and no upstream call: `update_agent_request`, `workspace.title.set.request`,
  `archive_workspace_request`, `archive_agent_request`, `delete_agent_request`, `refresh_agent_request` when the
  conversation is deleted (a restore), `agent_permission_response` (answering the agent's question is the person's);
- allows, and passes the viewer to magpie so it records before acting: `send_agent_message_request`,
  `workspace.create.request`, `cancel_agent_request`.
  magpie writes the audit row in the same transaction as the action; if it cannot, it does nothing and answers 503
  `view_as_not_recorded`, which pigeon turns into the error `Not sent: the view-as log could not be written. Try
  again.` (zh: `未能傳送：未能寫入代看記錄，請再試一次。`). Cancel is recorded too (today it is neither refused nor
  recorded, A14.F2 bug).
- magpie enforces the same refusals on its own API for any request carrying `Aviary-Viewer` (defence in depth,
  A17.F18).

### 6.5 Recording

All view-as audit lives in magpie (it owns conversation events; the audit names conversations): `start` (6.1),
`message` and `conversation` (with `chars` = text length, never content), `cancel`. Kept forever; the ops page reads
the latest 100 from magpie via hermit. The 39 existing `ops_events` rows of kind `view_as` are imported into magpie
once (magpie's import). The person is not notified (unchanged, `docs/CHAT.md:117-119`).

### 6.6 What the admin sees

Messages sent with the override look like the person's own (magpie stores them as the person's user rows with a
`viewer` field; pigeon does not show the viewer in the Paseo row, unchanged behaviour). The `me` answer includes
`viewAs` so the box shows who is being viewed and until when.

---

## 7. The Paseo protocol on `/paseo-ws`

The contract is the Paseo app's `WSOutboundMessageSchema` (`paseo/packages/protocol/src/messages.ts`), validated on
every frame by the client; a frame that fails is dropped silently (uncorrelated pushes) or fails its request
(correlated replies), A15.F13. pigeon must emit schema-exact payloads. Adding a field is safe (zod strips unknown
keys); a new message type, a new enum value, or a type change breaks shipped clients.

### 7.1 Transport

#### 7.1.1 Upgrade

`GET /paseo-ws` with `Upgrade: websocket`. Identity must be `Person` or `ViewAs` (via `?jarvis_view_as=`); else a
raw HTTP 401/403/503 response and close. No subprotocol. Permessage-deflate: off (Cloudflare compresses nothing on
WS; frames are mostly small JSON). Max inbound message 100 MiB (= the client's `MAX_WEBSOCKET_MESSAGE_BYTES`);
larger -> close 1009. Max inbound frame rate: 200 per second per socket sustained (burst 400); excess frames are read
and dropped with a warn log once per minute per socket.

#### 7.1.2 Socket state (memory only, dies with the socket)

```rust
struct Conn {
  id: Uuid,                       // for logs
  person: String,                 // email whose data this socket sees
  role: Role,                     // of the caller (admin/staff)
  viewer: Option<Viewer>,         // {admin, session} when view-as
  token_exp: i64,                 // Access token exp (and view-as e, the earlier)
  locale: Locale,
  hello_done: bool,
  agents_sub: Option<Uuid>,       // fetch_agents subscribe
  workspaces_sub: Option<Uuid>,   // fetch_workspaces subscribe
  projects_sub: Option<Uuid>,
  timeline_sub: Uuid,             // stable per connection
  timeline_agents: HashSet<Uuid>, // agent.timeline.set_subscription
  out: mpsc::Sender<Frame>,       // bounded, section 8.5
}
```

#### 7.1.3 Heartbeats

- Server: a WebSocket ping every 30 s; a socket that has not answered the previous ping is closed (A2.F1).
- Client: top-level `{"type":"ping"}` -> raw `{"type":"pong"}` (not in a session envelope), also before hello
  (A2.F2, A4.F3; the client pings every 10 s and reconnects after two 15 s misses).

#### 7.1.4 Re-checks on open sockets

Every 60 s per socket: the person (and for ViewAs, the admin and the view-as claims) are re-checked against hermit
(through the 15 s cache). No longer allowed -> close 4403 `not allowed`. When the Access token's `exp` (or view-as
`e`) passes -> close 4401 `signed out`. The client reconnects; the upgrade then fails at Access or pigeon, and the
app's hourly `/api/jarvis/me` check shows sign-in. (Today a socket outlives both; gap-1 3.1.)

### 7.2 Handshake

Client: `{"type":"hello","clientId","clientType","protocolVersion":1,"capabilities":{...},"appVersion"?}`.

- `protocolVersion` not in the accepted set (today `{1}`; the rename of 1.2 adds 2) ->
  `{"type":"hello.rejected","reason":"incompatible_protocol","accepts":[]}` then close
  1002. (The client shows 「Incompatible protocol version」; this is the forced-upgrade lever, A15.F11.)
- Otherwise `hello_done = true` and reply (in the session envelope):

```json
{ "type": "session", "message": { "type": "status", "payload": {
  "status": "server_info", "protocolVersion": 1, "serverId": "srv_conversations",
  "hostname": "<product.name>", "version": "<the hello's appVersion if it matches ^\\d+\\.\\d+\\.\\d+$, else 0.13.2>",
  "sourceId": "owner", "permissions": ["daemon.read","workspace.read","workspace.write"], "desktopManaged": false,
  "capabilities": { "voice": { "dictation": {"enabled": false, "reason": ""}, "voice": {"enabled": false, "reason": ""} } },
  "features": { "ownedSubscriptions": true, "explicitEventSubscriptions": true, "selectiveAgentTimeline": true,
    "providersSnapshot": true, "providersSnapshotCwd": true, "directorySync": true, "workspaceMultiplicity": true,
    "canonicalSubmittedPrompts": true, "agentTimelinePromptIndex": true, "workspaceSortTimes": true,
    "stableProjectIdentity": true, "projectList": true, "creationLifecycle": true, "agentRequestReceipts": true,
    "workspaceRequestReceipts": true } } } }
```
- `version` echoes the app's own version so the Settings "version mismatch" notice (`settings-screen.tsx:525-545`)
  never fires for a version pigeon accepts: pigeon is compatible with every app that speaks protocol 1, which is what
  that notice is meant to say. Every shipped client sends `0.13.2` today (A15.F12).
- Features deliberately absent (the app hides the UI): `forgeSearch`, `workspaceLabels`, `agentHistorySearch`,
  `projectedSubagentTimeline`, `fileByteWrites`, `fsEntryOps`, `checkout*`, `agentProfiles`, `daemonSelfUpdate`,
  `relayConfig`, `workspaceMarkUnread` (A4.F30).
- pigeon logs `clientType` and `appVersion` per connection (for a later real version gate; shipped clients cannot tell
  web from iOS, A15.F12).
- Session frames before hello are ignored silently (A2.F3).

### 7.3 Envelope and dispatch rules

- Every reply/push is `{"type":"session","message":{"type":<type>,"payload":{...}}}`, except `pong` and
  `hello.rejected` (top level). A send to a socket that is closing is skipped (A2.F4).
- Inbound: malformed JSON -> dropped. `{"type":"session","message":{...}}` -> dispatched by `message.type`.
  Unknown type with a non-empty `requestId` -> `rpc_error {requestId, requestType, error: "Not available here",
  code: "unsupported"}`; without `requestId` -> dropped silently (A2.F11, A4.F59).
- Handlers for one socket run concurrently (a slow timeline fetch must not block a ping), but replies are written
  through one ordered queue. A handler that fails unexpectedly answers `rpc_error {code:"failed", error:<localized
  unreachable sentence>}` when the request had a `requestId`; it never leaves a request unanswered (today a throwing
  create gave no reply, A2.F25).
- Every upstream call made for a request has a timeout well under the client's 60 s RPC timeout (section 13).

### 7.4 Fixed ids and constants

| Thing | Value |
|---|---|
| serverId | `srv_conversations` (cached hosts and replica rows are keyed by it: never change) |
| project | `projectId` = `projectKey` = `prj_conversations`, display name = `product.name`, root `/workspace`, kind `directory` |
| workspace id | `wks_<conversationId>` |
| agent id | the conversation id (UUID, lower case). Imported conversations keep their CloudCLI session id (requirement on magpie's import), so deep links `/h/srv_conversations/workspace/wks_<id>` and replica caches stay valid |
| cwd / workspaceDirectory | `/workspace` |
| provider | `claude` |
| untitled name | zh `新對話`, en `New conversation` |

### 7.5 Every client message, and what pigeon does

`P` = the socket's person headers (`Aviary-Person`, plus `Aviary-Viewer`/`Aviary-View-Session` for view-as) on the
magpie call. Magpie endpoints are numbered as in 14.1 (all under `/v1`). Every magpie call that changes a
conversation goes through the migration retry of 7.10. Shapes of `WorkspaceDescriptor`, `AgentSnapshot`,
`ProjectPlacement`, `PaseoRow` are in 7.6-7.8.

#### 7.5.1 Static and developer-machine messages (no upstream, A2.F5-F10, A4.F29)

| Client message | Reply payload (`message.type`) |
|---|---|
| `client_heartbeat`, `abort_request`, `audio_played`, `register_push_token` | none (ignored; presence is not routed anywhere, gap-3 #14-15) |
| `subscription.release.request {subscriptionId}` | `subscription.release.response {requestId, subscriptionId}`; if it names one of the socket's subscriptions, that subscription is cleared (agents/workspaces/projects: no more list pushes for it; timeline: `timeline_agents` emptied). Today it was a no-op (A2.F6) |
| `session.events.set_subscription.request` | `session.events.set_subscription.response {requestId, subscriptionId: <new uuid>}`; no events are ever emitted |
| `workspace.label.list.request {subscribe?, sync?}` | `workspace.label.list.response {requestId, subscriptionId? (only when subscribe), labels: [], sync: {mode: sync ? "changes" : "snapshot", generation: <list generation>, headSeq: 0, removals: []}}` |
| `hosts.list.request` | `hosts.list.response {requestId, subscriptionId: <uuid>, directory: {self: {serverId: "srv_conversations", label: <product>}, hosts: []}}` |
| `usage.list_reports.request` | `usage.list_reports.response {requestId, error: null}` |
| `plugin.catalog.get.request` | `plugin.catalog.get.response {requestId, plugins: []}` |
| `project.icon.get.request {projectId}` | `project.icon.get.response {requestId, projectId, icon: null, error: null}` |
| `project_icon_request {cwd}` | `project_icon_response {requestId, cwd, icon: null, error: null}` |
| `list_provider_features_request` | `list_provider_features_response {requestId, provider: "claude", features: [], error: null, fetchedAt: <now ISO>}` |
| `get_daemon_config_request` | `get_daemon_config_response {requestId, config: DAEMON_CONFIG}` (the exact object of `paseo-connection.ts:72-95`: relay/mcp/browserTools/plugins off, dictation/voiceMode off, providers `{}`, trustedProxies `["loopback"]`, idleRuntimeCleanup true, the rest empty/false) |
| `checkout_status_request {cwd}` | `checkout_status_response {requestId, cwd, isGit: false, repoRoot: null, currentBranch: null, isDirty: null, baseRef: null, aheadBehind: null, upstreamRef: null, aheadOfOrigin: null, behindOfOrigin: null, hasRemote: false, remoteUrl: null, isPaseoOwnedWorktree: false, error: null}` |
| `checkout_pr_status_request {cwd}` | `checkout_pr_status_response {requestId, cwd, status: null, githubFeaturesEnabled: false, authState: "no_remote", error: null}` |
| `subscribe_terminals_request {cwd, workspaceId}` | `terminals_changed {requestId, cwd, workspaceId, terminals: [], subscriptionId: <uuid>}` |
| `list_terminals_request {cwd}` | `list_terminals_response {requestId, cwd, terminals: []}` |
| `workspace.script.list.request {workspaceId}` | `workspace.script.list.response {requestId, workspaceId, scripts: [], error: null}` |
| `agent.provider_subagents.list.request {parentAgentId}` | `agent.provider_subagents.list.response {requestId, parentAgentId, subagents: [], error: null}` |
| `get_providers_snapshot_request {cwd?}` | `get_providers_snapshot_response {requestId, cwd?, generatedAt, entries: [], snapshotHash: "model:<id>", fetchedAt: {claude: <ISO>}, compactSnapshot: {thinkingSets: [], entries: [{provider: "claude", status: "ready", enabled: true, source: "builtin", label: <the default model's label>, description: "", defaultModeId: null, models: [{id, label, description: "", isDefault: true}], modes: []}]}}` from the cached `model` list (5.5). If `model` is unreachable with nothing cached: the same with `models: []` and `status: "ready"` (the composer then shows no label; sending still works because magpie, not the app, picks the model) |
| `project.list.request {subscribe?, sync?}` | `project.list.response {requestId, projects: [ProjectDescriptor], sync: {generation, headSeq, mode: "snapshot", reason: <non-empty sync ? "cursor_expired" : "no_cursor">, removals: []}}`; `subscribe` stores `projects_sub` (no project pushes ever) |

`ProjectDescriptor` = `{projectId: "prj_conversations", projectKey: "prj_conversations", projectDisplayName:
<product>, projectCustomName: null, projectCustomIconRevision: null, projectIconRevision: "automatic:none:v1",
projectRootPath: "/workspace", projectKind: "directory"}`.

#### 7.5.2 Conversation list

**`fetch_workspaces_request {subscribe?, sort?, page?: {limit, cursor?}, sync?}`** and **`fetch_agents_request
{scope?, sort?, page?, sync?, subscribe?}`** (A2.F14, A2.F15, A4.F7):
- Upstream: M1 `GET /v1/conversations?state=active&limit=<page.limit clamped 1..200, default 200>&cursor=<page.cursor>`
  with P. Sort is always activity (`updatedAt`) descending; the request's `sort` is ignored.
- The two requests arrive together on connect; per replica a single-flight with a 2 s micro-cache keyed by
  (person, cursor, limit) serves both from one magpie call.
- Reply `fetch_workspaces_response {requestId, subscriptionId: subscribe ? workspaces_sub : null, entries:
  [WorkspaceDescriptor], emptyProjects: (first page and no rows) ? [ProjectDescriptor] : [], pageInfo: {nextCursor:
  <magpie nextCursor>, prevCursor: null, hasMore: nextCursor != null}, sync: {generation, headSeq, mode: "snapshot",
  reason, removals: []}}` and `fetch_agents_response {requestId, subscriptionId, entries: [{agent: AgentSnapshot,
  project: ProjectPlacement}], pageInfo, sync}`. The list is now paged (the app follows `nextCursor` while `hasMore`,
  page 200; gap-0 14.6).
- `generation` = magpie's list generation for the person (a stable string), `headSeq` = the list `headSeq` magpie
  returned. Mode is always `snapshot`; the client's `sync` cursors are not used for deltas (the snapshot replaces its
  baseline, A4.F8). Because generation is stable, a reconnect no longer looks like a server restart (A2.F35, A14.F12).
- `subscribe` -> the socket joins the person's change feed from `headSeq` (8.3) and gets `workspace_update` /
  `agent_update` pushes (7.6.4).
- After `fetch_agents_response`, for each listed conversation whose `sandbox` is `preparing`, pigeon pushes
  `yyc.sandbox_status {agentId, state: "preparing"}`, so a tab that connects while a sandbox is being made shows the
  chip (gap-3 2.4).
- magpie error -> `rpc_error {code: "failed", error: <localized unreachable>}`.

**`fetch_agent_request {agentId}`** (A2.F16, A4.F25): M2 `GET /v1/conversations/{id}` (deleted included). Reply
`fetch_agent_response {requestId, agent: AgentSnapshot | null, project: ProjectPlacement | null, error: null |
"Conversation not found"}`. A non-UUID agentId is "not found" without a call.

**`fetch_agent_history_request {search?, sort?, page?: {limit, cursor?}}`** (Recently deleted, A2.F17, A4.F24): M1
`GET /v1/conversations?state=deleted&search=<trimmed>&limit=<clamp 1..200, default 50>&cursor=` (newest deletion first;
search is a case-insensitive title substring in magpie, one query, no N+1). Reply `fetch_agent_history_response
{requestId, entries: [{agent, project}], pageInfo: {nextCursor, prevCursor: null, hasMore}}`. Each agent has
`archivedAt` = `deletedAt`; the app computes "Deleted for good in N days" from it (30-day retention is magpie's).

#### 7.5.3 Timeline

**`agent.timeline.set_subscription.request {agentIds}`** (A2.F18, A4.F11): REPLACES `timeline_agents` with the given
ids (UUIDs only; at most 64 per socket, extra ids ignored). Reply `agent.timeline.set_subscription.response
{requestId, agentIds, subscriptionId: timeline_sub}`. Joining/leaving per-conversation timeline feeds follows (8.2).
No upstream call in the handler.

**`fetch_agent_timeline_request {agentId, direction: "tail"|"before"|"after", cursor?: {epoch, seq}, limit?,
projection?, mergeWindow?}`** (A2.F19, A4.F10, A4.F12, A4.F14):
- limit: default 40, clamped 1..500; `0` -> 500.
- Upstream M9 `GET /v1/conversations/{id}/timeline` with `tail=1&limit=L`, `before=<cursor.seq>&limit=L`, or
  `after=<cursor.seq>&limit=L` (no `waitMs`: an RPC never waits). Deleted conversations are readable.
- If `cursor.epoch` differs from the epoch magpie returns: pigeon discards that page, fetches `tail=1&limit=L`, and
  answers with `reset: true, staleCursor: true` and `direction` as requested (the app drops its cache and repaints).
- Reply `fetch_agent_timeline_response {requestId, agentId, agent: AgentSnapshot (from M2, fetched concurrently),
  direction, projection: "projected", epoch, reset, staleCursor, gap: false, window: {minSeq, maxSeq, nextSeq: maxSeq + 1},
  startCursor: rows ? {epoch, seq: rows[0].seqStart} : null, endCursor: rows ? {epoch, seq: rows[-1].seqEnd} : null,
  hasOlder, hasNewer, entries: [PaseoRow], error: null}` plus `mergeWindow` echoed when the request had it.
- `after` returns rows with `seqEnd > cursor.seq` (so an updated tool row reappears); `before` returns rows with
  `seqStart < cursor.seq`. These are magpie's semantics (M9, 14.1.18); pigeon only projects.
- Unknown conversation (M2/M9 404): `{requestId, agentId, agent: null, direction, projection: "projected", epoch: "",
  reset: false, staleCursor: false, gap: false, window: {minSeq: 0, maxSeq: 0, nextSeq: 0}, startCursor: null,
  endCursor: null, hasOlder: false, hasNewer: false, entries: [], error: "Conversation not found"}`.
- Cost is proportional to the page (magpie reads rows by seq; target design "History ... cost ~ page size").

**`agent.timeline.list_prompts.request {agentId}`** (A2.F22, A4.F15): M10. Reply
`agent.timeline.list_prompts.response {requestId, agentId, epoch, prompts: [{seq, timestamp, preview (≤ 200
chars)}], error: null}`; 404 -> `prompts: [], error: "Conversation not found"`.

**`agent.timeline.search.request`**: stays `unsupported` (feature `agentHistorySearch` absent; the find bar searches
loaded rows, A4.F16). A later magpie search can light it up.

#### 7.5.4 Send

**`send_agent_message_request {agentId, text, messageId, images?: [{data, mimeType}], attachments?:
[{type: "uploaded_file", id, fileName, mimeType, size, path}], activeTurnBehavior?}`** (A2.F23, A4.F17):
1. Images: each `data` (base64) is decoded and streamed to M13 `POST /v1/uploads` with `Aviary-Upload-Kind: image`,
   `Content-Type: <mimeType>`. Decoding happens in 1 MiB slices, never a second full copy. Any image upload failing ->
   the send fails (`accepted: false`, the localized upload error) and nothing is sent.
2. M7 `POST /v1/conversations/{agentId}/turns` with P, body `{"clientMessageId": messageId (or a fresh UUID if absent),
   "text": text, "images": [{"uploadId"}], "files": [{"path", "name", "mimeType", "size"}]}`, 20 s timeout.
3. Map the answer to `send_agent_message_response {requestId, agentId, accepted, error}`:

| magpie | accepted | error |
|---|---|---|
| 202 or 200 (repeat of the same clientMessageId) | true | null |
| 409 `turn_running` | false | zh `對話仍在進行中，請稍候。` / en `This conversation is still replying. Please wait.` |
| 409 `conversation_deleted` | false | zh `這個對話已刪除。` / en `This conversation was deleted.` |
| 404 | false | `Conversation not found` |
| 503 `view_as_not_recorded` | false | 6.4 sentence |
| 409 `conversation_migrating` still after the 45 s retry of 7.10 (usually the retry succeeds) | false | the 7.10 "being moved" sentence |
| 413/415 | false | 5.7 sentences |
| timeout / 5xx / network | false | zh `暫時連接不到這個對話，請再試一次。` / en `Could not reach this conversation. Please try again.` |

- `activeTurnBehavior` and `sessionMode` are ignored; the app queues on its own while a reply runs (A4.F18). There is
  no server queue.
- Admission is magpie's, atomically (one turn per conversation; two concurrent sends from two sockets: one 202, one
  409; fixes A14.F10). A send while background work is held but no turn runs is admitted and the supervisor
  supersedes the hold, with no confirmation (read-only product rule; A9.F7). A send while a CLI-started follow-up turn
  runs gets 409 like any running turn.
- No optimistic server row, no synthetic `turn_started`, no echo dedupe in pigeon: magpie stores the user row with
  `clientMessageId` and the turn start; both arrive through the feeds (8.2, 8.3) within milliseconds, and the app
  replaces its optimistic row when the server row carries the same `clientMessageId` (A14.F6, A14.F7).
- Images over the socket are bounded by the 100 MiB frame; at most 3 inbound frames larger than 8 MiB are processed
  at once per replica (a semaphore; further large frames wait, which pauses reading that socket).

#### 7.5.5 New conversation

**`workspace.create.request {requestId, idempotencyKey, subscribe?, source?, agent?: {config, initialPrompt,
clientMessageId, images?, attachments?}, firstAgentContext?: {prompt, images?, attachments?}}`** (A2.F25, A4.F20):
1. `text` = `agent.initialPrompt` else `firstAgentContext.prompt` else ``; `clientMessageId` = `agent.clientMessageId`
   else `<idempotencyKey>:initial-message`; images/attachments from `agent` else `firstAgentContext`. Images uploaded
   as in 7.5.4.
2. M3 `POST /v1/conversations` with P, `{"idempotencyKey", "message": {"clientMessageId", "text", "images", "files"}}`,
   20 s timeout. magpie keys idempotency on (person, idempotencyKey) durably, so a retry after a reconnect, or from
   another replica, returns the same conversation (fixes A14.F7; today the map was per socket). In view-as magpie
   records the `conversation` audit in the same transaction; failure = nothing created.
3. On 201/200 `{conversation, turn}`: if `subscribe`, push five `workspace.create.update` frames `{kind: "workspace",
   idempotencyKey, revision: 0..4, phase: accepted, workspace_ready, agent_ready, prompt_started, completed, error:
   null, workspaceId, agentId, workspace: WorkspaceDescriptor, agent: AgentSnapshot}` (the app navigates on the first
   with a workspace), then `workspace.create.response {requestId, workspace, agent, setupTerminalId: null, error:
   null}`. The list upsert reaches every socket through the change feed. A new conversation is never migrating
   (cutover 6.1), so M3 needs no 7.10 retry.
4. Errors -> `workspace.create.response {requestId, workspace: null, agent: null, setupTerminalId: null, error}`
   (view-as: 6.4 sentence; otherwise zh `未能開始對話，請再試一次。` / en `Could not start the conversation. Please try
   again.`), and with `subscribe` one `workspace.create.update {..., revision: 0, phase: "failed", workspaceId: null,
   agentId: null, error, errorCode: <magpie code>}` first.

**`creation.subscribe.request {kind, idempotencyKey, subscribe?}`** (sent on reconnect mid-create; today unsupported,
so the creation failed client-side, A4.F20): M4 `GET /v1/creations/{idempotencyKey}` with P. Found -> `creation.subscribe.response
{requestId, subscriptionId?: <uuid when subscribe>, snapshot: {kind: "workspace", idempotencyKey, revision: 4, phase:
"completed", workspaceId, agentId, workspace, agent, error: null}, error: null}`; 404 -> `snapshot: null, error: null`
(the app then re-sends the create with the same key, which magpie de-duplicates).

#### 7.5.6 Stop

**`cancel_agent_request {agentId}`** (A2.F24, A4.F19, A9.F8, A16.F14): M8 `POST /v1/conversations/{id}/cancel` with P
(view-as allowed, recorded by magpie). Reply at once with `cancel_agent_response {requestId, agentId, agent:
AgentSnapshot | null (M2 after the cancel), error: null}`; the real end arrives later as `turn_canceled` on the feed.
The cancel is conversation-level: pigeon names no turn, because the app's Stop names none and a hold has no turn.
magpie answers 202 `{turnId}` in every state: a running turn is stopped (8.6 of magpie: interrupt, kill after 30 s);
during a background hold (no active turn, Claude Code's tasks still running) magpie queues the supervisor's
`stop_tasks` command and answers `{turnId: null}`; with nothing running it is a no-op answering `{turnId: null}`. It
never answers "no active turn". pigeon never turns a cancel into an error row (today `NO_ACTIVE_RUN` became an error
row plus `turn_failed`, A9.F12). magpie is responsible for the turn ending within seconds even when the sandbox is
gone (A16.F14, gap-0 14.2). A cancel of a migrating conversation goes through 7.10 and, if still refused, answers
`cancel_agent_response` with `error` = the 7.10 "still finishing" sentence.

#### 7.5.7 Rename

- **`update_agent_request {agentId, title}`** (A2.F26, A4.F22): trimmed empty title -> reply `accepted: true, error:
  null` without a call (today's behaviour). Else M5 `PATCH /v1/conversations/{id} {"title"}`. Reply `update_agent_response
  {requestId, agentId, accepted, error}`.
- **`workspace.title.set.request {workspaceId, title}`**: `wks_` prefix stripped (else "not found"); empty -> error zh
  `名稱不能留空。` / en `The name can't be empty.` without a call. Else M5. Reply `workspace.title.set.response
  {requestId, workspaceId, accepted, title: accepted ? title : null, error}`.
- Title max 200 characters (longer is cut by pigeon). A rename is magpie's `titleSource: "user"` and is never
  overwritten by generated titles. Other tabs see it through the change feed.

#### 7.5.8 Delete, delete for good, restore

- **`archive_workspace_request {workspaceId}`**, **`archive_agent_request {agentId}`** (A2.F27, A4.F23): M6 `POST
  /v1/conversations/{id}/delete` (soft, into Recently deleted). Replies: `archive_workspace_response {requestId,
  workspaceId, archivedAt: <deletedAt> | null, error}`; `agent_archived {requestId, agentId, archivedAt}` or `rpc_error
  {code: "failed"}`.
- **`delete_agent_request {agentId}`**: M2 first. Already deleted -> `DELETE /v1/conversations/{id}` (for good: magpie
  purges rows, blobs, shares, sent snapshots); not deleted -> soft delete as above. Reply `agent_deleted {requestId,
  agentId}` or `rpc_error {code: "failed", error}`.
- **`refresh_agent_request {agentId}`** (restore, A2.F28, A4.F24): M2; if deleted, M6 `POST /v1/conversations/{id}/restore`.
  Then `status {status: "agent_refreshed", agentId, requestId}` (a `status` message, not a `_response`). A refresh of a
  live conversation does nothing else. The list push comes from the change feed.
- All refused in view-as (6.4).

#### 7.5.9 The agent's questions (AskUserQuestion / ExitPlanMode)

If magpie reports a pending ask on a conversation (14.1 `Conversation.asks`, change `ask`), pigeon:
- lists it in `AgentSnapshot.pendingPermissions` as `{id: ask.id, provider: "claude", name: ask.toolName, kind:
  ask.toolName == "AskUserQuestion" ? "question" : ask.toolName == "ExitPlanMode" ? "plan" : "tool", title?,
  description?, input: ask.input}` and sets `requiresAttention: true, attentionReason: "permission",
  attentionTimestamp: ask.createdAt`;
- pushes `agent_permission_request {agentId, request}` to the person's sockets subscribed to the list or the timeline,
  and `agent_permission_resolved {agentId, requestId: ask.id, resolution}` when it is answered or cancelled;
- handles **`agent_permission_response {agentId, requestId, response}`** (behavior allow with `updatedInput` (the
  answers) / deny with `message`): M11 `POST /v1/conversations/{id}/asks/{askId}/answer` with the response, its
  `updatedInput.answers` keyed by each question's text: the app's card keys an answer by the question's `header`,
  Claude Code and the supervisor's parked-answer message read it by the text (rehearsal R1, 2026-11-06: "Blue" keyed
  by its header resumed as "The user did not answer the questions."); a key that is already a text stays. Refused in
  view-as. No reply message (the protocol has none); failures are logged.

This replaces today's silent hang (A11.F8, A11.F11, gap-3 2.7). The supervisor may well never produce asks in YYC
(disallowing these tools fits a read-only product); pigeon's relay is generic and costs nothing when unused.

A question waits for as long as the person takes and never times out for them (DECISIONS 17): the sandbox is released
while the question waits, magpie keeps the turn (parked) and the ask, so the question stays in `pendingPermissions`
and the turn stays running (Stop still cancels it). The answer goes through M11 as above; the reply follows a little
later, once a new sandbox has started. The working-time counter stands still meanwhile (7.7).

#### 7.5.10 Everything else

All other session message types the app can send go to the `rpc_error unsupported` default, unchanged from today:
agent.create, create_agent_request, resume_agent_request, import_agent_request, agent.rewind, agent.fork_context,
agent.timeline.search/append, set_agent_model/mode/thinking/feature, list_provider_models/modes,
list_available_providers, refresh_providers_snapshot, wait_for_finish, close_items, every checkout_*/stash/branch
request, terminals beyond the two stubs, fs.*, file_explorer, file.upload, file_download_token, plugin.*,
hosts.add/update/remove/route/link, workspace.pin.set, workspace.mark_unread, workspace.clear_attention,
clear_agent_attention, workspace.label.* writes, workspace.recovery.*, workspace.setup.*, workspace.script.start/stop,
project.add/remove/rename/icon.set/create_directory/github.clone, daemon.*, speech.*, push.unregister, diagnostics,
provider.usage.list, schedule.* (A2.F11). Files, uploads and downloads are REST (section 5).

### 7.6 Server pushes

#### 7.6.1 Timeline rows: `agent_stream` (timeline)

For each new or updated row of a conversation in the socket's `timeline_agents`:
`agent_stream {agentId, event: {type: "timeline", provider: "claude", item: <PaseoItem>, turnId?}, timestamp:
row.timestamp, seq: row.seqEnd, epoch, subscriptionId: timeline_sub}` (A2.F30). Rows arrive in `seqEnd` order. An
updated tool row (its result arrived) is re-sent with its new `seqEnd`. Live rows of conversations no socket
subscribes to are not fetched at all (they are read as history when opened). Text arrives one whole content block
at a time, as today (8.7: no token-level frames in this release).

#### 7.6.2 Turn lifecycle: `agent_stream` (turn_*)

From change-feed `turn` entries (8.3), to sockets whose `timeline_agents` contain the conversation (A2.F31, A14.F9):
`agent_stream {agentId, event: {type: "turn_started" | "turn_completed" | "turn_failed" | "turn_canceled", provider:
"claude", turnId, error? (failed: the localized sentence of 7.9 for `error.code`, default zh `這次回覆失敗。` / en
`The turn failed.`), code? (failed: the magpie code), reason? (canceled: "Stopped")}, timestamp, subscriptionId}` (no
seq, no epoch). Every turn has a start and an end, including turns the agent starts itself (background follow-ups)
and turns started from another tab, device or replica: magpie records explicit boundaries (A9.F5, A9.F12, gap-3
2.6). Ordering with rows: 8.4.

#### 7.6.3 `agent.timeline.replacement`

When magpie reports `timeline_replaced {conversationId, epoch}` (history rewritten, e.g. Claude Code rewrote a
transcript in place): `agent.timeline.replacement {subscriptionId: timeline_sub, agentId, epoch}` to sockets
subscribed to that conversation; the app refetches the tail with reset (A4.F11, A14.F8).

#### 7.6.4 List updates: `workspace_update`, `agent_update`

To sockets of the person with `workspaces_sub` / `agents_sub`, for each change-feed entry (A2.F29):
- `conversation_upserted {conversation}`: if `conversation.deletedAt` is null: `agent_update {subscriptionId:
  agents_sub, kind: "upsert", agent: AgentSnapshot, project: ProjectPlacement, generation, seq}` and
  `workspace_update {subscriptionId: workspaces_sub, kind: "upsert", workspace: {...WorkspaceDescriptor, syncSeq:
  seq}, generation, seq}`; if deleted: the two removes below.
- `conversation_removed {conversationId}`: `workspace_update {kind: "remove", id: "wks_<id>", generation, seq}` and
  `agent_update {kind: "remove", agentId, generation, seq}`.
- `seq` = the change's list seq (the same number on both frames). The client only keeps the highest seq as its cursor
  (`directory-sync/index.ts:926-946`), so equal seqs are fine; per-socket gaps no longer happen (A14.F12).
- Titles (provisional -> generated -> user renames), running/done, activity re-sorting and deletions all arrive this
  way (gap-3 #1, #6, #7).

#### 7.6.5 `yyc.sandbox_status`

A wire constant of the shipped app fork (1.2: it is the message type the app listens for, not YYC behaviour; the
state it carries is any hermit implementation's). From change-feed `sandbox {conversationId, state: "preparing" |
"ready"}`: `yyc.sandbox_status {agentId, state}` to
EVERY socket of the person, not filtered by subscription (as today, A2.F32, A14.F11). Plus the on-connect replay in
7.5.2. A turn that ends while preparing: magpie emits `ready` (or pigeon sends `ready` for that conversation on any
turn end, idempotent).

#### 7.6.6 Asks

`agent_permission_request` / `agent_permission_resolved`, 7.5.9.

### 7.7 Object mappings from magpie's `Conversation` (14.1.0)

Let `c` be a magpie Conversation, `name = c.title ?? <untitled>`, `running = c.turn != null`. `c.migrating` is not
shown (a migrating conversation looks as it does on the old system; its writes are retried, 7.10).

`ProjectPlacement(c)` = `{projectKey: "prj_conversations", projectName: <product>, workspaceName: name, checkout:
{cwd: "/workspace", isGit: false, currentBranch: null, remoteUrl: null, worktreeRoot: null, isPaseoOwnedWorktree:
false, mainRepoRoot: null}}`.

`WorkspaceDescriptor(c)` = `{id: "wks_"+c.id, projectId: "prj_conversations", projectDisplayName: <product>,
projectCustomName: null, projectCustomIconRevision: null, projectRootPath: "/workspace", workspaceDirectory:
"/workspace", setupStatus: null, projectKind: "directory", workspaceKind: "directory", name, title: c.title, pinnedAt:
null, background: false, archivingAt: null, status: running ? "running" : "done", statusEnteredAt: c.updatedAt,
activityAt: c.updatedAt, createdAt: c.createdAt, lastUserMessageAt: c.lastUserMessageAt ?? c.updatedAt, diffStat:
null, scripts: [], project: ProjectPlacement(c)}`.

`AgentSnapshot(c)` = `{id: c.id, provider: "claude", cwd: "/workspace", workspaceId: "wks_"+c.id, model: <default
model id or null>, thinkingOptionId: null, effectiveThinkingOptionId: null, runtimeInfo: {provider: "claude",
sessionId: c.claudeSessionId, model: <same>, modeId: null}, createdAt: c.createdAt, updatedAt: c.updatedAt,
lastUserMessageAt: c.lastUserMessageAt ?? c.updatedAt, status: running ? "running" : "idle", activeTurn: running ?
{turnId: c.turn.id, startedAt: c.asks.length ? null : c.turn.startedAt + c.turn.askWaitMs} : null, capabilities: {supportsStreaming: true,
supportsSessionPersistence: true, supportsSessionListing: false, supportsDynamicModes: false, supportsMcpServers:
false, supportsReasoningStream: true, supportsToolInvocations: true, supportsRewindConversation: false,
supportsRewindFiles: false, supportsRewindBoth: false}, currentModeId: null, availableModes: [], features: [],
pendingPermissions: <7.5.9>, persistence: null, title: name, labels: {}, archivedAt: c.deletedAt, requiresAttention:
asks non-empty, attentionReason: asks ? "permission" : null, attentionTimestamp: asks ? first ask createdAt : null}`.

`activeTurn.startedAt` is what the app's working-time counter runs from (`now - startedAt`): the turn's start moved
on by the time its answered questions waited, and null while a question is open (the app then shows no counter), so
the counter stands still while the person decides and goes on from where it was after the answer (DECISIONS 17).

All timestamps ISO-8601 UTC with milliseconds (`2026-11-02T03:46:12.345Z`).

### 7.8 Timeline row projection (magpie row -> Paseo row)

magpie serves timeline rows in a protocol-neutral schema (14.1.0 `Row`, model in 14.1.18). pigeon's projection is a
pure function of one row plus the locale, the conversation id and the blob key, so every replica produces the same
bytes and no projection state is kept. Stateful work (deciding what is a row, pairing a tool result with its call,
the row seq, the epoch, classifying model errors) is magpie's, done once when the event is stored.

`PaseoRow` = `{provider: "claude", item, turnId?, timestamp, seqStart, seqEnd, sourceSeqRanges, collapsed}`
(`sourceSeqRanges` = the row's `seqs` as ranges, so a tool row that took seq 10 and, with its result, seq 15 carries
`[{startSeq: 10, endSeq: 10}, {startSeq: 15, endSeq: 15}]`; `collapsed` is `["tool_lifecycle"]` when the row has
more than one seq, else `[]`).

| magpie `item.kind` | Paseo `item` |
|---|---|
| `user` `{messageId, clientMessageId?, text, images: [{blobId, mimeType}], files: [{path, name, mimeType, size}], viewer?}` | `{type: "user_message", text, messageId, clientMessageId?, images?: [{url: signed(blobId,"display"), mimeType, thumbnailUrl: signed(blobId,"thumb")}], attachments?: [{type: "uploaded_file", id: path, fileName: name, mimeType (from the extension table if empty), size, path}]}`. `thumbnailUrl` is a new optional field old clients strip; a later app build can use it for the "thumbnail first, full on tap" behaviour of the target design. `viewer` is not shown. |
| `assistant` `{messageId, text, error?: ErrorInfo}` | `{type: "assistant_message", text, messageId}`. When magpie tagged the text as a model API error (`error.code` `model.*`, classified once by magpie from Claude Code's `API Error: …` line, live and imported alike, A1.F24), `text` is replaced by the 7.9 sentence for `error.code` (`model.budget` and `model.refused` use `error.message`, egress's or the provider's own words). Empty text -> no row (magpie does not make one). |
| `reasoning` `{text}` | `{type: "reasoning", text}` |
| `tool` `{callId, name, input, status, result?: {text, truncated, bytes, blobId?, isError}, sentFiles?, error?}` | `{type: "tool_call", callId, name, status: running/completed/failed/canceled, error, detail: <mapper>, metadata?}`. `detail` comes from a Rust port of the vendored Paseo mapper (`server/modules/paseo/vendor/claude-tool-call-mapper.ts`, `claude-tool-call-detail-parser.ts`, `tool-call-detail-primitives.ts`, Apache-2.0, keep the notice): shell / read / edit / write / search / fetch / plan / plain_text / sub_agent / unknown, from `name`, `input` and `result.text`. `name` is the call's own, trimmed, as the vendored mapper's `raw.name.trim()`: an MCP tool keeps its server, so history written as `mcp__yyc__*` and new `mcp__tools__*` calls (supervisor 6.4) show the names staff see on the old stack today (cutover 10.2 R6; an earlier reading of this row dropped the server part, which renamed 391 imported calls and gave `mcp__*__web_search` a search detail the old bridge never showed); the mapper has no list keyed by a server name, and pigeon has no allow/deny list of tools (tool policy is the supervisor's). `metadata.sentFiles` = `[{path, name, size, mediaType, isImage, sent: callId}]` for SendUserFile (A2.F21; the app shows download cards). When `result.truncated` (magpie kept the full output in the bucket and sent a preview), the shown text ends with a localized line zh `…（輸出過長，只顯示開頭部分）` / en `… (output too long; only the start is shown)`; the app has no "load the rest" control today, so pigeon adds none. |
| `error` `{code, message, detail?}` (a failed turn's row, 14.1.18) | `{type: "error", message: <7.9 sentence for code; `message` for codes without one>}` |
| anything else | no row (pigeon skips unknown kinds; magpie ships a new kind only after pigeon handles it) |

Paths: `files[].path` and `sentFiles[].path` are relative to `/workspace` (magpie guarantees no `..`, NUL or
absolute path; pigeon drops entries that violate it).

A frame larger than 16 MiB after projection (a pathological tool input) is cut: the largest string fields are
truncated to 1 MiB with the line above, so a client never receives more than 16 MiB in one push.

### 7.9 Strings

One table compiled into pigeon, `zh` (Traditional Chinese, HK usage) for `product.locale` starting `zh`, else `en`.
Codes are the dotted data codes magpie stores (platform 3.6: event error codes keep their dotted form): turn failures
(`turn.ended.error.code`, from magpie, hermit, the supervisor and the cut-over) and model-error tags on assistant
rows. pigeon has no regex over message text; the API-error rules of today's `jarvis-errors.ts:94-115` moved to
magpie's classifier.

| Code | zh | en |
|---|---|---|
| `sandbox.unreachable` | 暫時連接不到這個對話的電腦，請稍後再試。 | Could not reach the computer for this conversation. Please try again in a minute. |
| `sandbox.busy` (no capacity) | 所有電腦正在使用中，請過幾分鐘再試。 | All computers are busy right now. Please try again in a few minutes. |
| `sandbox.not_allowed` | 這個帳戶不可使用此服務，請聯絡管理員。 | This account is not allowed to use this service. Please ask your administrator. |
| `sandbox.budget` | 此服務的使用預算已用完，請聯絡管理員。 | The usage budget for this service has been used up. Please ask your administrator. |
| `sandbox.failed` (could not start) | 未能啟動這個對話的電腦，請再試一次；如問題持續，請通知管理員。 | Could not start the computer for this conversation. Please try again; if it keeps happening, tell your administrator. |
| `sandbox.timeout` (not ready in 15 min) | 準備這個對話的電腦用了太長時間，請再試一次。 | Preparing the computer for this conversation took too long. Please try again. |
| `sandbox.lost` (stopped mid-turn; new, fixes A16.F16's wrong "could not start") | 這個對話的電腦在回覆途中停止了，回覆未能完成。請再試一次。 | The computer for this conversation stopped during the reply, so it is incomplete. Please try again. |
| `run.exited`, `run.error`, `internal` | 這次回覆失敗，請再試一次。 | The reply failed. Please try again. |
| `run.oom` (supervisor 23 item 4) | 這次工作用盡了電腦的記憶體，回覆未能完成。請把工作拆細一點再試。 | This task ran out of the computer's memory, so the reply is incomplete. Please try a smaller step. |
| `stop.forced` | 已停止回覆。 | The reply was stopped. |
| `legacy.cut` (cutover 6.5) | 這個回覆在系統升級時中斷了，請再傳送一次。 | This reply was cut off by the system upgrade. Please send your message again. |
| `model.budget` | `message` if present (egress's own localized sentence), else 此服務的使用預算已用完，請聯絡管理員。 | `message`, else The usage budget for this service has been used up. Please ask your administrator. |
| `model.rate_limited` | 目前向 AI 模型發出的請求太多，請稍等一會再試。 | Too many requests to the AI model right now. Please wait a moment and try again. |
| `model.unavailable` | AI 模型目前繁忙或暫時無法使用，請稍後再試。 | The AI model is busy or unavailable right now. Please try again in a moment. |
| `model.access` | AI 模型服務拒絕存取，請通知管理員。 | The AI model service refused access. Please tell your administrator. |
| `model.refused` | AI 模型拒絕了這個請求：{message} | The AI model refused this request: {message} |
| `model.connection` | 連接不到 AI 模型，請再試一次。 | Could not reach the AI model. Please try again. |
| other | `message` as given | `message` as given |

A code pigeon does not know falls to "other"; a new code magpie or another component adds ships with its row here
(the contract suite has one fixture per code).

Fixed server strings (A2.F40): untitled 新對話 / New conversation; busy send, empty rename (above); `Conversation not
found`, `Not available here`, `Could not start the conversation` stay English where today's code is English (the app
shows them rarely and the tests assert them).

### 7.10 Migration refusals (the switch window)

During the switch a conversation imported from the old system is `migrating` until the importer hands it over
(cutover 6). magpie refuses every change to it with 409 `{"code": "conversation_migrating", "message",
"retryAfterMs": 2000 | 5000}` (platform 3.6 shape). pigeon's `upstream` layer handles it once for every changing call
(M5, M6, M7, M8, M11, M16 create; M3 never sees it):
- wait `retryAfterMs` (clamped 500..10 000) and repeat the identical call (same `clientMessageId` for M7, so magpie's
  idempotency makes the repeat safe), until 45 s have passed since the app's request; then answer the app with the
  sentence below. No attempt starts after 45 s, and each attempt's timeout is cut to what is left of 55 s, so the
  app (RPC timeout 60 s) always gets an answer.
- The retry lives in the request handler's memory and ends with the socket; the app's own retry after a reconnect is
  safe for the same reason. View-as refusals (6.3, 6.4) come first, without any magpie call.
- Reads (lists, timeline, files, blobs, shares) and a share's continue (its copy is born owned, cutover 6.4) are
  never refused for migration.

| Action | zh | en |
|---|---|---|
| send, rename, delete, restore, delete for good, share create | 此對話正在搬到新系統，請一分鐘後再試。 | This conversation is being moved to the new system. Please try again in a minute. |
| stop, answer | 上一個回覆仍在舊系統完成中，暫時未能停止，最多 15 分鐘內會自動結束。 | The previous reply is still finishing on the old system and can't be stopped yet. It ends by itself within 15 minutes. |

When the importer marks a still-running old turn on a migrating conversation (cutover 6.6), the conversation shows
`turn` non-null, so the app shows the spinner and disables send as for any running turn; its end arrives as an
ordinary `turn` change, `legacy.cut` when the old system cut it (7.9). The whole section is generic: any deployment
that imports conversations uses it, and outside an import no conversation is migrating. It is deleted with magpie's
`migrating` column after the switch (cutover 13.3: pigeon first, then magpie).

---

## 8. Live streaming from magpie to sockets

### 8.1 Two feeds

magpie keeps live fan-out between its replicas internal (Postgres LISTEN/NOTIFY, never exposed). pigeon reads two
REST long-poll feeds:

1. **Timeline feed, per conversation**: M9 `GET /v1/conversations/{id}/timeline?after=<seq>&limit=200&waitMs=25000`.
   magpie answers at once if rows with `seqEnd > after` exist, else holds the request until one does or 25 s pass
   (then `rows: []`). Same endpoint and semantics as history paging; `waitMs` only adds the hold. The rows are
   magpie's timeline rows (14.1.18), whose seq is dense per conversation and epoch.
2. **Change feed, per person**: M12 `GET /v1/changes?after=<seq>&limit=500&waitMs=25000` with `Aviary-Person`. Entries:
   `conversation_upserted`, `conversation_removed`, `turn`, `sandbox`, `ask`, `ask_resolved`, `timeline_replaced`,
   each with the person's change `seq` (increasing per person; gaps are allowed, because magpie may collapse
   superseded upserts of one conversation; pigeon only needs "after").

Long-poll (rather than a held NDJSON stream) keeps every magpie request ≤ 25 s, so magpie drains in seconds and any
magpie replica can answer the next poll: no affinity in either direction. A draining magpie replica answers its held
polls at once with what it has (possibly nothing, platform 3.3); pigeon's next poll lands on another replica.

### 8.2 Pollers

Per pigeon replica, pollers are in-memory and shared by all sockets on that replica:
- `TimelinePoller(person, conversationId)` exists while ≥ 1 local socket of that person has the conversation in
  `timeline_agents`, and 30 s after the last one leaves (absorbs reconnect flaps). It starts by reading M9 `tail=1&limit=1`
  for the current `maxSeq` and `epoch`, then polls `after=maxSeq`. A poll answer with a different `epoch` than the
  poller's (history rewritten; normally announced first by `timeline_replaced`) makes the poller send
  `agent.timeline.replacement` (7.6.3) to its sockets and restart from M9 `tail=1&limit=1` in the new epoch; it never
  emits rows of the new epoch under the old one.
- `ChangePoller(person)` exists while ≥ 1 local socket of that person is open, plus 30 s. It starts at the `headSeq` of
  the first list snapshot taken on this replica (or M12 `after=latest` which returns the current head with no
  entries), then polls.
- Each poller has its own HTTP request; 200 conversations open on a replica = 200 held requests to magpie. Limit: 4 000
  pollers per replica (13); beyond that new subscriptions on that replica are served without live rows (the client's
  gap detection and visibility catch-ups still fetch them) and a metric/alert fires.
- Errors: exponential backoff 0.5 s doubling to 5 s, forever; the cursor is kept, so nothing is missed after magpie
  recovers. 404 on a timeline poll (conversation purged) stops the poller. 410 `cursor_expired` on the change feed
  (only after a long outage): the poller restarts at the current head and every local socket of that person is closed
  with 1012 so it re-snapshots (correctness over cleverness).

### 8.3 Joining a feed without losing or duplicating

- **Change feed.** A socket that subscribes takes a list snapshot (7.5.2) with `headSeq = H`. The replica's
  `ChangePoller(person)` is at position `P`.
  - `P >= H`: entries `H < seq <= P` already went by; pigeon fetches them once for this socket (M12 `after=H&limit=500`,
    no wait, repeated until it reaches P) and sends them, then the socket receives live entries `> P`.
  - `P < H`: the socket ignores live entries with `seq <= H` (they are in its snapshot).
- **Timeline feed.** No join logic: the app gates rows by seq itself (`seq == endSeq + 1` accepted, `<=` dropped,
  `>` = gap -> it fetches `after=endSeq`, A4.F11). A socket joining a running poller may see its first live row as a
  gap and catch up by one RPC; that is the protocol's own mechanism.

### 8.4 Ordering of turn events against rows

`turn` entries carry `timelineSeq`: the conversation's timeline `maxSeq` when the turn event was committed. pigeon
delivers a turn event for conversation X to a socket only after it has delivered X's rows up to `timelineSeq` on this
replica (the change poller hands the event to X's timeline poller, which releases it after emitting that seq). If the
rows have not arrived within 2 s, the event is sent anyway. So `turn_completed` follows the reply's last row, and the
app's queue drains after a synchronised timeline (A4.F18). Sockets not subscribed to X's timeline do not get turn
events, only the list upsert.

### 8.5 Fan-out and backpressure

- A poller result is serialised once per (projection, locale) and the same bytes go to every socket subscribed.
- Each socket has a bounded outbound queue (16 MiB or 4 096 frames). A full queue closes the socket with 1013 (try
  again later) and a warn log; the client reconnects and refetches. A slow socket never slows a poller or another
  socket.
- Writes to a socket are serialised through its queue; ordering per socket is the order frames were enqueued.

### 8.6 Throughput and latency targets

- Feed latency (magpie commit -> frame on the socket): p50 ≤ 50 ms, p99 ≤ 300 ms inside the cluster.
- One replica (1 CPU) handles 5 000 sockets and 2 000 frames/s of fan-out. YYC today: ~20 staff, tens of sockets.

### 8.7 No token-level live frames in this release

Text reaches the app one completed content block at a time, from magpie's rows, which is what staff see today (no
`--include-partial-messages` today, A11). Token streaming is left out of this release on purpose:
- the Paseo app merges consecutive `assistant_message` items and gates sequenced timeline frames by a dense seq, so
  live chunks need their own unsequenced path that the durable row then replaces (the app's
  `replaceLiveAssistantWithProjectedText` does this only when the durable text starts with the live text), plus a
  mid-message join for a socket that opens during a block, a supersede rule that holds across magpie rollouts and
  pigeon reconnects, and a contract test running the app's real reducer;
- none of magpie, pigeon or the harness specifies that today, and getting it wrong shows replies twice.
So the supervisor does not pass `--include-partial-messages` and sends no live frames, magpie has no `/live` route,
and platform E2 does not assert live frames (section 22). Adding streaming later is additive: a `live` entry kind on
M9 (`{messageId, index, text}` without a seq, superseded by the row carrying that `messageId`), a `GET
/v1/conversations/{id}/live` for a mid-message join, pigeon sending them as seq-less `agent_stream` timeline
`assistant_message` frames, and a contract test asserting the rendered text equals the durable text with no
duplicates, all behind a new `magpie-person` protocol number (platform 3.4).

---

## 9. Reconnect and resume by seq

What happens when a socket drops (network, pigeon drain, pigeon crash), on any replica:

1. The client backs off (1.5 s doubling to 30 s), reconnects through the Service to any ready replica, sends hello,
   gets server_info, re-sends all owned subscriptions (`fetch_workspaces`/`fetch_agents` subscribe,
   `project.list`, `agent.timeline.set_subscription`, `session.events...`, `workspace.label.list`) and flushes queued
   sends (A4.F1, A4.F2, A4.F5, A4.F6).
2. The list comes back as a snapshot from magpie (stable generation, current `headSeq`), and the change feed resumes
   after it (8.3).
3. For each visible conversation the app sends `fetch_agent_timeline_request {direction: "after", cursor: {epoch,
   seq: endSeq}}`. magpie's epoch is stable across pigeon replicas and magpie restarts (it changes only when history
   is rewritten), so the answer is exactly the rows the app missed: a running turn's rows continue where they stopped,
   with nothing lost and nothing doubled (target design: "a reconnect resumes by seq"; fixes A16.F0, A16.F3).
4. Turn state comes back in the agent snapshot (`status`, `activeTurn`), so the spinner and Stop button are right even
   if `turn_started` was missed. `yyc.sandbox_status` preparing is replayed after the list (7.5.2).
5. An RPC in flight when the socket dropped is rejected by the client (`DAEMON_CONNECTION_LOST`). Sends are
   idempotent by `clientMessageId` and creates by `idempotencyKey` in magpie, so a retry never duplicates; a
   create interrupted mid-way is re-attached with `creation.subscribe.request` (7.5.5).

The turn itself is unaffected by any of this: it runs in the sandbox under the supervisor and is stored in magpie.

---

## 10. Multi-replica statelessness

- Memory per replica: open sockets and their subscriptions; pollers; caches (JWKS, verified tokens, people, first
  names, deployment facts, model list, the rendered shell, warm coalescing, list micro-cache). All are rebuildable
  from upstream; losing them costs latency, never correctness.
- No pigeon-to-pigeon traffic, no peer list, no rendezvous hashing, no holder lookups, no forwarded sockets: the
  whole of `router.mjs`'s placement layer goes (A0.F24, A7.F13, A16.F3).
- No affinity: the Service balances connections freely; the same person's tabs may sit on different replicas and see
  the same data because it all comes from magpie.
- Identity propagation to upstreams (platform 3.5): `Authorization: Bearer <pair token>` proves the caller is pigeon;
  `Aviary-Person` says whose data; `Aviary-Viewer` + `Aviary-View-Session` mark view-as; `Aviary-Request: <uuid v7>`
  (minted by pigeon at the edge, one per HTTP request or socket RPC) traces one request across services (logged by
  each as `request`); `Aviary-Protocol: magpie-person=1` / `hermit-core=1` names the contract version. Every
  client-supplied `Aviary-*` header is stripped before anything else. magpie and hermit trust these headers only with
  pigeon's token.
- Autoscaling: HPA on CPU (target 70 %), min 2, max 6. Sockets are long-lived, so new replicas take new connections
  only; that is fine at this scale.

---

## 11. Drain and rollout (zero downtime)

### 11.1 Probes

- `GET :9090/healthz` -> 200 `{"ok":true}` while the process runs its event loop (liveness).
- `GET :9090/readyz` -> 200 when: deployment facts loaded at least once, every team's JWKS loaded at least once, the
  app bundle loaded, and not draining; else 503 with `{"ok":false,"why":[...]}`. Readiness does NOT depend on magpie or
  hermit being up (that would take every replica out at once during a magpie blip; pigeon answers with clear errors
  instead).

### 11.2 Drain on SIGTERM

1. `preStop: sleep 5` runs first (the pod leaves the Service endpoints and cloudflared's connection pool stops sending
   new requests).
2. On SIGTERM: `draining = true`, `/readyz` 503, stop accepting new TCP connections on 8080 (the listener closes; any
   straggler connects to another replica).
3. Open sockets: each gets close 1012 (`service restart`) at a random moment within the next 15 s (spread so the
   clients' reconnects do not land at once), after its in-flight RPCs have replied or 10 s have passed. The close is
   written only between frames: never inside an outbound frame (frames are written whole), and, when an inbound
   message is being received (a large image send arrives as one message of up to 100 MiB), only after that message
   has been read completely and its send handled, so a deploy never cuts a send in the middle. That wait has the
   transfer bound of step 4. Sends already accepted by magpie are safe (the reply may be lost; section 9 step 5).
4. In-flight HTTP requests are never cut by a drain while they make progress: an upload (5.7) or a download (5.8,
   5.9, shares) that has started runs to completion, up to the transfer bound of 300 s after SIGTERM, with the
   ordinary 60 s idle timeout still applying (a 50 MiB upload at 2 Mbit/s takes 210 s). Ops pages and small REST
   calls finish in seconds. Nothing new starts on the old replica: the listener closed in step 2, and keep-alive
   connections get `connection: close` on their next answer.
5. Pollers are cancelled (no state to save). The process exits 0 with log `drained` as soon as no socket and no
   request is left, normally within 15-20 s; only a slow transfer keeps it longer.
6. `terminationGracePeriodSeconds: 320` (= preStop 5 + transfer bound 300 + 15, platform 3.3's rule).

A rollout therefore costs each open tab one reconnect (≈ 1.5-3 s without frames, then a seq catch-up), never
interrupts a turn, and never fails an upload or download that is moving. Resumable (chunked) uploads would let a
replica exit sooner, but they need an app change and magpie upload sessions for a case a 300 s bound already covers;
they are not built. There is nothing to wait for: today's 900 s "keep running chats" drain (A0.F30, A7.F12, A9.F15,
A16.F0, A16.F2) exists only because turns lived in the gateway's child process; they now live in the supervisor.

### 11.3 Rollout shape (YYC manifests)

Deployment `pigeon`: replicas 2 (HPA 2-6), `strategy: RollingUpdate {maxSurge: 1, maxUnavailable: 0}`,
`minReadySeconds: 5`, `revisionHistoryLimit: 3`, PDB `minAvailable: 1`, topology spread `maxSkew: 1` per hostname
with `matchLabelKeys: [pod-template-hash]`, `priorityClassName: serving`, no node affinity (pigeon needs no JuiceFS;
it can run on the small `cx23` nodes). Readiness every 2 s (failureThreshold 1), liveness every 10 s
(failureThreshold 3, initialDelay 5 s). (A0.F31, A20.F23.)

---

## 12. Failure modes

| Failure | What staff see | pigeon behaviour |
|---|---|---|
| A pigeon replica dies (OOM, node loss) | Socket drops; reconnect in 1.5-3 s; the reply continues from where it was | none needed; 9 |
| magpie down or all magpie replicas draining | Open tabs stay connected; new RPCs fail with the localized "could not reach" error; live rows pause, then catch up | upstream timeouts (13), pollers back off and resume from their cursor; readiness unaffected |
| magpie slow (> RPC timeout) | the request errors; the app shows the error (sends: retry is safe) | timeouts below the app's 60 s |
| hermit down | Signed-in people keep working for 5 min (stale people cache); new people get 503 「暫時未能使用」; warm hints dropped; `/ops` 502 | 4.4 stale-on-error; deployment facts kept |
| hermit says nobody is allowed (offline) | Everyone gets 403 within 15 s; open sockets close within 75 s | 4.4, 7.1.4 |
| Cloudflare JWKS unreachable | Nothing while the cached keys are valid (1 h, kept beyond on failure); a key rotation during the outage locks out new tokens | keep last JWKS; refetch with cooldown |
| get-identity unreachable | Greeting without a name | firstName null, 5 min retry |
| `model` unreachable | Model label missing in the composer | 5.5; sending unaffected |
| An app build too old (protocol ≠ 1) | 「Incompatible protocol version」 | 7.2 |
| A bundle missing a chunk an open tab wants | The app's chunk retry, then one reload per 10 min (A15.F5) | bundles keep recent builds' chunks (17.1); a 404 is no-store |
| A huge inbound frame storm | Large sends wait a little | large-frame semaphore (7.5.4) |
| A slow client | That tab reconnects | 8.5 |
| Clock skew | — | ±5 s leeway on exp/nbf |
| View-as audit write fails | "Not sent: the view-as log could not be written" | 6.4 |
| Image upload fails mid-send | The send fails with the upload error; nothing sent | 7.5.4 |
| pigeon rollout or scale-down during an upload | The upload finishes; the tab then reconnects once | 11.2 step 4 |
| A conversation still being moved at the switch | A send or rename starts a few seconds late; at worst the "being moved" sentence after 45 s | 7.10 |

---

## 13. Sizes, limits and timeouts

| Item | Value |
|---|---|
| Inbound WS message | ≤ 100 MiB (close 1009 beyond) |
| Large inbound frames (> 8 MiB) processed at once per replica | 3 |
| Outbound push frame | ≤ 16 MiB (7.8 truncation) |
| Per-socket outbound queue | 16 MiB or 4 096 frames (close 1013) |
| Sockets per replica | 10 000 (upgrade 503 beyond) |
| Sockets per person per replica | 64 (oldest closed 1008 beyond) |
| `timeline_agents` per socket | 64 |
| Pollers per replica | 4 000 |
| Inbound frame rate per socket | 200/s sustained, 400 burst |
| Upload | file part ≤ 52 428 800 bytes (50 MiB); whole multipart body ≤ 53 477 376 bytes (50 MiB + 1 MiB overhead) |
| JWT length | 8 KiB |
| View-as token | 2 048 characters, 12 h |
| View-as form / small JSON bodies | 4 096 bytes |
| Title | 200 characters |
| Path parameters | `path` ≤ 4 096 characters; ids UUID; `sent` `^[A-Za-z0-9_-]{1,128}$`; `state` `^[A-Za-z0-9_-]{16,128}$` |
| Timeline page | default 40, 1..500 |
| List page | default 200 (active), 50 (deleted), max 200 |
| Upstream timeouts | magpie RPC 15 s; magpie turn/create 20 s; migration retry (7.10) 45 s in total; magpie long-poll `waitMs` 25 s with a 35 s client timeout; magpie view-as record 5 s; uploads/downloads: 30 s to first byte, then 60 s idle; hermit people 5 s; hermit warm 10 s; hermit ops 30 s; hermit deployment 5 s; model 5 s; JWKS 5 s; get-identity 1.5 s |
| Caches | JWKS 1 h (unknown kid refetch ≥ 30 s); verified token 60 s; people 15 s (+300 s stale on error); first name 6 h / 5 min; deployment 60 s; model list 60 s; warm 30 s per key, 1 000 keys; list micro-cache 2 s |
| Heartbeats | server WS ping 30 s; client ping 10 s answered at once; open-socket re-check 60 s |
| HTTP server | header read 30 s; idle keep-alive 120 s |
| Drain | preStop 5 s; socket close jitter 15 s; in-flight RPCs 10 s; transfers (uploads, downloads, a large inbound send) up to 300 s; grace 320 s |
| Memory | request = limit 384 Mi (platform 9.4, 16.1); CPU request 100m, limit 1 |
| Ephemeral | app bundle emptyDir ≤ 512 Mi |

(magpie's own cap is the same 52 428 800 bytes of file; pigeon's guards only stop it streaming more.)

---

## 14. Interfaces pigeon consumes (exact shapes)

These are the contracts the magpie and hermit specs must provide; their types live in the contract crates
`magpie-api` and `hermit-api` (platform 4.4), which pigeon's client and magpie's/hermit's servers both compile against,
so a disagreement is a build error, not a production bug. This section is the single definition of magpie's person
surface (platform R3); magpie's spec states the same routes and shapes (section 22.1 lists what it must change).

Every call carries `Authorization: Bearer <pair token>`, `Aviary-Request: <uuid v7>` and `Aviary-Protocol:
magpie-person=1` (or `hermit-core=1`); person-scoped calls carry `Aviary-Person: <email>`, and view-as calls also
`Aviary-Viewer: <admin email>` and `Aviary-View-Session: <session>`. Bodies and query parameters are camelCase.
Errors are `{"code": "<lower_snake_case>", "message": "<English, for logs>", …type-specific fields}` with the HTTP
status shown (platform 3.6). Data codes inside rows and turns (`sandbox.lost`, `model.budget`) are dotted and are not
HTTP codes.

### 14.1 magpie (`http://magpie`, all routes under `/v1`)

#### 14.1.0 Types

```text
Conversation {
  id: uuid, title: string|null, titleSource: "provisional"|"generated"|"user"|null,
  createdAt, updatedAt (last activity), lastUserMessageAt: iso|null, deletedAt: iso|null, purgeAt: iso|null,
  claudeSessionId: string|null,
  turn: { id: string, startedAt: iso, origin: "user"|"cli"|"import", askWaitMs: number } | null,   // non-null = running
  lastTurn: { id, outcome: "completed"|"failed"|"canceled", endedAt: iso, error: ErrorInfo|null } | null,
  sandbox: "preparing"|"ready"|null,
  asks: [ Ask ],
  migrating: bool                       // true only while the switch's importer owns it (cutover 6)
}
Ask { id: string, toolName: string, toolUseId: string, input: object, title?: string, description?: string, createdAt: iso }
ErrorInfo { code: string, message: string, status?: int, detail?: string }
Row {
  seqStart: int, seqEnd: int,           // first and latest row seq this row took
  seqs: [int],                          // every row seq it took, ascending (1 for most rows; a tool row 2-3)
  timestamp: iso, turnId?: string,
  item: UserItem | AssistantItem | ReasoningItem | ToolItem | ErrorItem          // shapes in 7.8
}
Change { seq: int, at: iso, type, conversationId, ...type-specific fields (below) }
```

`origin`: `user` (a person's send through pigeon), `cli` (a turn Claude Code started itself, e.g. a background
follow-up; the supervisor's `origin: "cli"`), `import` (an old-system turn shown during the switch, cutover 6.6). One
enum for magpie, the supervisor and pigeon; pigeon does not display it.

Timeline seq requirement (hard): within a conversation and epoch, every new row and every row update takes exactly
the next integer (dense, starting at 1), and nothing else takes one. The app treats any jump as a gap and refetches
(`session-stream-reducers.ts` `classifySessionTimelineSeq`), so a counter that also counted events that make no
visible row would turn every live row into a catch-up. magpie's event seq is a different counter (14.1.18).

#### 14.1.1-14.1.17 Endpoints

| # | Call | Request | Success | Errors |
|---|---|---|---|---|
| M1 | `GET /v1/conversations` | `state=active\|deleted`, `limit` 1..200, `cursor` (opaque), `search` (deleted only) | 200 `{conversations: [Conversation], nextCursor: string\|null, list: {generation: string, headSeq: int}}` ordered by `updatedAt` desc (active) / `deletedAt` desc (deleted); `headSeq` is the person's change seq read in the same snapshot | 400 `bad_cursor` |
| M2 | `GET /v1/conversations/{id}` | | 200 `Conversation` (deleted included; owner only) | 404 `not_found` |
| M3 | `POST /v1/conversations` | `{idempotencyKey, message: {clientMessageId, text, images: [{uploadId}], files: [{path,name,mimeType,size}]}}` | 201 `{conversation, turn: {id, startedAt}\|null}`; 200 same body for a repeated key | 503 `view_as_not_recorded`; 400 `bad_request` / `unknown_upload` (after the legacy-path resolution of magpie M7, also for M3); 413 `too_large`; 415 `unsupported_type` |
| M4 | `GET /v1/creations/{idempotencyKey}` | | 200 `{conversation, turn}` (the person's create with that key) | 404 `not_found` |
| M5 | `PATCH /v1/conversations/{id}` | `{title}` (1..200 characters after trimming) | 200 `Conversation` (`titleSource: "user"`) | 400 `empty_title` / `title_too_long`; 403 `view_as_read_only`; 404; 409 `conversation_migrating` |
| M6 | `POST /v1/conversations/{id}/delete`; `POST /v1/conversations/{id}/restore`; `DELETE /v1/conversations/{id}` | | 200 `Conversation` (with `deletedAt`, `purgeAt`); 200 `Conversation`; 204 | 409 `not_deleted` (DELETE of one not in Recently deleted); 403 `view_as_read_only`; 404; 409 `conversation_migrating` |
| M7 | `POST /v1/conversations/{id}/turns` | `{clientMessageId, text, images: [{uploadId}], files: [{path, name, mimeType, size}]}` | 202 `{turn: {id, startedAt}}`; 200 same for a repeated clientMessageId | 409 `turn_running` / `conversation_deleted` / `conversation_migrating` (`route: "legacy"`: re-routed to LU, never shown); 404; 503 `view_as_not_recorded`; 400 `unknown_upload` (an old asset path is first resolved through `uploads.legacy_path`, `import_uploads` and a synchronous ingest; magpie M7); 413/415 |
| M8 | `POST /v1/conversations/{id}/cancel` | `{}` | 202 `{turnId: string\|null}`: stops the active turn; during a hold queues the supervisor's `stop_tasks`; otherwise a no-op. Recorded (`cancel`) when `Aviary-Viewer` is present | 404; 409 `conversation_migrating` |
| M9 | `GET /v1/conversations/{id}/timeline` | one of `tail=1` / `before=S` / `after=S`; `limit` 1..500; `waitMs` 0..25000 (only with `after`) | 200 `{epoch: string, minSeq, maxSeq, hasOlder, hasNewer, rows: [Row]}`; `after` = rows with `seqEnd > S` ascending by `seqEnd`; `before` = rows with `seqStart < S` (the last `limit` of them, ascending by `seqStart`); `tail` = the last `limit` rows by `seqStart`; stops early at 4 MiB of JSON (`hasNewer`/`hasOlder` say so) | 404 |
| M10 | `GET /v1/conversations/{id}/prompts` | | 200 `{epoch, prompts: [{seq, timestamp, preview}]}`; `seq` = the user row's `seqStart` (row seq space) | 404 |
| M11 | `POST /v1/conversations/{id}/asks/{askId}/answer` | Paseo `AgentPermissionResponse`, answers keyed by question text (7.5.9) | 202 `{}` | 409 `already_resolved` / `conversation_migrating`; 403 `view_as_read_only`; 404 |
| M12 | `GET /v1/changes` | `after` int or `latest`; `limit` 1..500; `waitMs` 0..25000 | 200 `{generation, headSeq, changes: [Change]}` | 410 `cursor_expired` |
| M13 | `POST /v1/uploads` | body = bytes (chunked); headers `Content-Type`, `Aviary-Upload-Name` (percent-encoded), `Aviary-Upload-Kind: file\|image` | 201 `{uploadId, path, name, size, mimeType}`; `path` is unique among the person's unattached uploads and is what M3/M7 `files[].path` name | 413 `too_large` (cap 52 428 800); 415 `unsupported_type` |
| M14 | `GET /v1/conversations/{id}/files/raw` | `path`, `sent?`, `inline?`; `Range` etc. | 200/206 bytes + headers (5.8) | 400/404 JSON |
| M15 | `GET /v1/blobs/{blobId}` | `variant=thumb\|display\|full`; no person header (pigeon verified the capability) | 200 bytes, `content-type`, `etag`; `thumb` = long edge 512 px WebP made on first request and kept | 404 |
| M16 | shares: `GET /v1/conversations/{id}/shares`, `POST /v1/conversations/{id}/shares`, `DELETE /v1/shares/{shareId}`, `GET /v1/shares/{shareId}`, `GET /v1/shares/{shareId}/files/raw`, `POST /v1/shares/{shareId}/continue` | | 200 `{shares: [{id, createdAt}]}`; 201 `{id, createdAt}`; 204; 200 `{id, title, ownerName, isOwner, createdAt, epoch, rows: [Row]}` (every row up to the share point, inline); bytes; 201 `{conversationId}` | 409 `nothing_to_share`; 404 `not_found`; 403 `view_as_read_only`; 409 `conversation_migrating` (create only) |
| M17 | `POST /v1/view-as` | `{admin, person, session, expiresAt}` | 201 `{}` | 5xx |

`conversation_migrating` carries `retryAfterMs` (2000, or 5000 while an old turn runs): `{"code":
"conversation_migrating", "message": "conversation is being moved", "retryAfterMs": 2000}` (7.10).

`Change` types: `conversation_upserted {conversation}`, `conversation_removed {reason: "deleted"|"purged"}`, `turn
{turnId, phase: "started"|"completed"|"failed"|"canceled", origin, error?: ErrorInfo, timelineSeq: int}`, `sandbox
{state: "preparing"|"ready"}`, `ask {ask: Ask}`, `ask_resolved {askId, resolution: AgentPermissionResponse}`,
`timeline_replaced {epoch}`. A soft delete emits `conversation_upserted` with `deletedAt` set (pigeon turns that into
removes) and a restore emits `conversation_upserted` with `deletedAt: null`. `timelineSeq` = the conversation's
timeline `maxSeq` in the transaction that committed the turn event. Changes are kept 30 days; an older `after`
answers 410.

Behaviour pigeon relies on (for the magpie spec): owner checks on every person-scoped call (404 for another person's
conversation); idempotency of M3 by (person, idempotencyKey) and M7 by (conversation, clientMessageId), durable;
atomic turn admission; view-as audit in the same transaction as the action (send, create, cancel) and refusals of
rename/delete/restore/answer/share create/revoke/continue when `Aviary-Viewer` is present; explicit turn boundaries
for every turn including agent-started ones; a turn always ends (failed with a code) within seconds of the sandbox
being lost; the row model of 14.1.18; error codes from 7.9; every change of 14.1.18 committed in the same
transaction as the event that causes it.

#### 14.1.18 The timeline row model (magpie owns it)

The rows pigeon streams and pages are magpie's, kept beside the events, because only the owner of the events can
number rows densely and statelessly for every reader (23.1). What magpie must guarantee, whatever its storage:

1. **Two counters.** Events keep their own `seq` (dense, every event, magpie 7). Timeline rows have a row seq per
   (conversation, epoch), dense from 1, advanced only by an event that creates a row or changes one. Both are
   assigned in the same transaction, under the per-conversation lock that already serialises event appends, so a
   reader never sees an event without its rows or a row seq that is later reused.
2. **What makes a row** (the derivation is one magpie function, used for live writes, imports, shares and rebuilds):

| Event | Row effect |
|---|---|
| `user.sent` | new `user` row (`messageId`, `clientMessageId`, text, images by blob id, files by workspace path, `viewer`) |
| `cc.user` prompt line that echoes a `user.sent` | none (the user row exists) |
| `cc.user` prompt line with no `user.sent` (imported history, a prompt Claude Code queued itself) | new `user` row from the line (import display fixes applied, magpie 19.3) |
| `cc.assistant` text block | new `assistant` row; when the line is Claude Code's `API Error: …` text, `error` = the classified model code (`model.*`, with `status` and the provider's or egress's `message`) |
| `cc.assistant` thinking block | new `reasoning` row |
| `cc.assistant` tool_use block | new `tool` row, `status: "running"` |
| `cc.user` tool_result for a call | updates that call's `tool` row (status, `result` preview, `truncated`, `blobId` of the full output): next row seq appended to `seqs` |
| `files.sent` | updates the call's `tool` row (`sentFiles`): next row seq |
| `turn.ended` with `outcome: "failed"` | new `error` row `{code, message, detail?}` |
| `turn.ended` canceled: a tool row still `running` | updated to `canceled` (next row seq) |
| every other kind (`conversation.created`, `turn.started`, `turn.stop_requested`, `turn.ended` completed, `sandbox.*`, `title.changed`, `task.status`, `ask.*`, cc lines that show nothing) | none, and no row seq |

3. **Epoch.** An opaque string, stable across magpie restarts, replicas and rollouts. It changes only when the
   conversation's rows are rebuilt: a `retract` (Claude Code rewrote history, magpie 6.7) or a magpie release whose
   derivation differs from the stored rows' (a `derivationVersion` per conversation; rebuilt lazily on first read).
   A rebuild re-derives every row from the events in one transaction, numbers them from 1 in a new epoch, deletes
   the old epoch's rows and writes `timeline_replaced {epoch}` to the person's changes. Cost is proportional to the
   conversation, and it is rare (tombstone rewrites; none in a normal turn; compaction appends, it does not rewrite).
4. **Paging** reads rows by `(conversation, epoch, seqEnd)` and `(conversation, epoch, seqStart)` indexes: cost is
   proportional to the page (target design "cost ~ page size"); a blob stays a reference.
5. **Long-poll.** M9 with `waitMs` and M12 with `waitMs` wait on magpie's internal wake-up (LISTEN/NOTIFY, never
   exposed) and re-query; they answer at once when anything is past `after`, else at `waitMs` with nothing, and at
   once when the replica drains.
6. **Shares and continue.** A share's `rows` are the rows derived from the events up to the share's cut (magpie
   12.1), in the share's own epoch; a continued conversation gets its own rows (copied with their seqs, new epoch).
7. **Import.** The importer writes rows with the events (one derivation); an imported conversation starts in a fresh
   epoch, so a tab holding a CloudCLI epoch repaints once (cutover 12).
8. **The change feed.** `person_changes` (or magpie's name for it) rows are written in the same transaction as the
   event or state change they describe; a `turn` change's `timelineSeq` is read in that transaction. Consecutive
   `conversation_upserted` entries of one conversation may be collapsed into the latest (seq gaps allowed); `turn`,
   `ask`, `ask_resolved`, `sandbox` and `timeline_replaced` entries are never collapsed.

### 14.2 hermit (`http://hermit`)

| # | Call | Success | Notes |
|---|---|---|---|
| H1 | `GET /v1/deployment` | 200 `{product: {name, locale}, signIn: {access: [{teamDomain, audience}], appScheme: string\|null}, …}` (pigeon ignores other fields) | 4.1 |
| H2 | `GET /v1/people/{email}` | 200 `{email, allowed: bool, role: "admin"\|"staff"\|null}` | re-read live (no restart for an allowlist change); 4.4 |
| H3 | `POST /v1/warm` | 202 | body `{person, conversationId: uuid\|null}`; a demand hint, never creates or touches a sandbox (gap-3 #18) |
| H4 | `GET\|HEAD /ops/{rest}` | page or JSON | not under `/v1` (browser paths, platform 3.4); `Aviary-Person` = the admin; hermit re-checks role |

### 14.3 model (`http://model`)

`GET /v1/aviary/models` (platform R18, the same route the supervisor uses through the gateway) -> 200 `{"default":
"<id>", "small": "<id>", "title": "<id>", "contextTokens": int, "models": [{"id", "label", "reasoning"}]}`. pigeon
reads `default`, `models[].id` and `models[].label`; the model a turn uses is not pigeon's concern.

### 14.4 Cloudflare

`GET https://<team>/cdn-cgi/access/certs` (JWKS) and `GET https://<team>/cdn-cgi/access/get-identity` (cookie). Both
generic Cloudflare Access features; the team comes from H1.

---

## 15. CloudCLI's `/ws` and REST surface: where each piece goes

Everything below disappears from the public host with the CloudCLI server. "pigeon" = a pigeon route (section 5);
"magpie internal" = a magpie REST route reachable only inside the cluster (ops scripts use it with their own token
through `kubectl port-forward`); "gone" = no replacement.

| Today | Used by | New home |
|---|---|---|
| WS `/ws` (chat.send, chat.edit-send, chat.abort, chat.stop-task, chat.subscribe, chat.permission-response; pushes session_upserted/removed, loading_progress) | no YYC app; ops `ui-pod-chat`, the e2e harness, Jarvis 4 Chat (local mode) | gone from YYC. Its semantics live in magpie (turns, cancel, events by seq) and the supervisor (stop-task). ops scripts move to magpie internal REST (18.4). Jarvis 4 Chat: left open (dogfooding). pigeon has no `/ws` route: an upgrade there gets 404 (5.1 row 22) and a plain GET gets the shell; `/ws` leaves the Access app in the cut (17.5) |
| WS `/desktop-notifications` | none | gone |
| WS `/paseo-ws` | both apps | pigeon (section 7) |
| `GET /health`, router `/healthz`, `/readyz` | router, probes | pigeon `:9090` only |
| `/__jarvis/peer/holds` | router peers | gone (no peers) |
| `/__jarvis/sent-files/snapshot` | egress (SendUserFile hook) | magpie internal (egress -> magpie; magpie spec) |
| `/api/jarvis/me`, `signin`, `app-signin`, `view-as` | apps, ops page | pigeon |
| `/api/jarvis/warm` | apps | pigeon -> hermit `POST /v1/warm` |
| `/api/jarvis/sessions/:id/files/raw` | apps | pigeon -> magpie |
| `/api/jarvis/sessions/:id/files` (list) | ops script only | magpie internal |
| `/api/jarvis/sessions/:id/shares`, `/api/jarvis/shares/*` | apps | pigeon -> magpie |
| `/api/jarvis/*` local-mode routes | Jarvis 4 only | gone from YYC (Jarvis 4 open) |
| `POST /api/assets/files` | apps | pigeon -> magpie `POST /v1/uploads` |
| `POST /api/assets/images`, `GET /api/assets/images/:f`, `GET /api/assets/files/:f` | none | gone |
| `GET /api/providers/claude/models` | apps | pigeon (from `model`) |
| `GET /api/providers/sessions/running` | router (idle/drain), Jarvis 4 Chat | gone as a route; "is it running" is `Conversation.turn` in magpie |
| `POST /api/providers/sessions`, `GET .../sessions/:id/messages`, `DELETE .../sessions/:id` | ops script, e2e, Jarvis 4 Chat | magpie internal (`POST /v1/conversations`, timeline, delete) |
| other `/api/providers/*` (search, recent, deleted, restore, rename, token usage, fork, CLI auth status) | none in YYC | gone (the socket covers list/rename/delete/restore) |
| `/api/auth/*` | none | gone |
| `/api/projects*` | e2e print only | gone |
| `/api/commands/*` | none (and a cross-person read hole, A10.F7) | gone |
| `/api/settings/*` (notification prefs, push, VAPID, api-keys, credentials) | none | gone; no push in the target (gap-3) |
| `/api/notifications/*` | none | gone |
| `/api/system/*` | none | gone |
| `/api/user/*` (preferences, onboarding, drafts) | none | gone (drafts are on the device) |
| `/api/scheduled-messages`, queued-message dispatcher | none | gone |
| `/api/voice/*` | none | gone |
| `/api/files/download` | dead upstream explorer path | gone (404 `NOT_AVAILABLE`) |
| SPA catch-all, `dist/`, `public/sw.js` | browsers | pigeon static + shell (5.2) |
| `public/api-docs.html` | none | gone |
| `/ops/*` | admins | pigeon -> hermit |

---

## 16. Data pigeon owns

None. Specifically not carried over (all in CloudCLI's per-person schemas or the router's memory): the `users` table
and the "first DB user" platform auth (A10.F1, A12.F4: identity is the Access email), `app_config` and its JWT secret
(A12.F5), `vapid_keys` and `push_subscriptions` (A12.F14, A12.F15: no web push), notification preferences, router
process maps. The view-as audit and its 39 rows go to magpie (6.5).

---

## 17. Deployment (YYC)

### 17.1 The app bundle

Built by DE0CH/claudecodeui (the Paseo apps stay there) in a new workflow `pigeon-app.yml` on every push to main
touching `paseo/**`, `public/sw.js` or `scripts/build-paseo-ui.mjs`. It replaces `jarvis-image.yml` (deleted in the
switch's cleanup, cutover 15.2). The web export is heavy (A4.F57), so the job is sized for it:

- `runs-on: ubuntu-24.04` (the standard runner for public repos: 4 vCPU, 16 GiB RAM), `timeout-minutes: 45`. The
  export takes about 20 min on 4 cores; Metro's transform workers hold the 10 MB generated protocol validator, so
  the build needs a 6 GiB heap: `NODE_OPTIONS=--max-old-space-size=6144`, which `scripts/build-paseo-ui.mjs` sets
  itself (the workflow does not override `NODE_OPTIONS`). A smaller runner fails or is OOM-killed; that is the
  reason for this line, not a preference.
- Caches: `actions/setup-node` npm cache keyed on `paseo/package-lock.json`, and `actions/cache` for Metro's cache
  directory (`$TMPDIR/metro-cache`, key = lockfile hash + `paseo/packages/app/**` tree hash prefix), which brings
  incremental builds well under the cold 20 min.
- Steps:
  1. `actions/setup-node` with Node `22.23.3` (the version of today's `jarvis/Dockerfile`).
  2. `npm --prefix paseo ci`. Its `postinstall` (`paseo/scripts/postinstall-patches.mjs`) applies the patches,
     including the no-`__common` patch to `@expo/metro-config` (`paseo/patches/@expo+metro-config+57.0.13.patch`:
     no eager `__common` chunk, and async chunks drop modules their importers already define).
  3. Assert the patch applied: `grep -q 'const commonChunk = undefined'
     paseo/node_modules/@expo/metro-config/build/serializer/serializeChunks.js` (an Expo bump that silently stops
     the patch from applying would put lazy code back on the first load and break the loading budgets).
  4. `node scripts/build-paseo-ui.mjs`: the Expo web export (`expo export --platform web --max-workers 3`), the
     `index.html` stylesheet rewrite, and `scripts/check-chunks.mjs` (chunk sizes and loading budgets; a breach fails
     the job). Output `dist/`.
  5. Contract check against pigeon (18.1): download `pigeon-frames.ndjson` from the newest `aviary-*` release of
     DE0CH/aviary and validate every frame against this commit's `WSOutboundMessageSchema` and response schemas
     (`node testing/validate-frames.mjs` from the aviary release's `paseo-contract.tgz`). A protocol change in the app
     that would reject a frame pigeon sends fails the app build before anything is published.
  6. Merge in the hashed assets of recent builds: pull the previous `pigeon-app` image, copy its `_expo/static/**`
     files not present in the new build, keep a manifest `assets-history.json` `{file: firstSeenISO}`; prune files
     whose build is older than 30 days and not among the last 5 builds. The first build is seeded with the chunks of
     the live `yyc-ai-ui` image (`/app/dist/_expo/static/**` of `ghcr.io/de0ch/yyc-ai-ui:3de3f5d8771b`, cutover D13),
     so tabs loaded from `ui` keep loading their lazy chunks from pigeon after the switch.
  7. Write `.br` (quality 11) and `.gz` (9) siblings for js/css/html/json/svg/wasm. Add `public/sw.js`.
  8. Build and push `ghcr.io/de0ch/pigeon-app:<sha12>` (`FROM scratch`, files under `/app`, plus the static `/cp`).
     Public package (platform 5.5).
  9. Append this commit to `testing/paseo-contract/clients.txt` in aviary through a `repository_dispatch` to
     aviary's `clients.yml`, which commits the line to aviary main (a plain push, never forced); `yyc-ios.yml` does
     the same for every TestFlight upload (18.1).
- The `index.html` the export writes is served as built except for the two substitutions of 5.2.2; the early message
  box and the page frame (A4.F47, A4.F48) live in it.
- The pigeon pod's initContainer (same image, `command: ["/cp", "-a", "/app/.", "/srv/app/"]`) fills an `emptyDir`
  (`sizeLimit: 512Mi`) mounted read-only into pigeon at `/srv/app`.
- So a tab loaded from build N keeps fetching N's chunks from any replica of build N+1 (fixes A15.F0, A15.F4, A15.F7,
  A15.F8, A15.F9 in practice; the app's retry/reload logic, A15.F5-F6, stays as the backstop).
- YYC's `yyc-ai/deploy/kustomization.yaml` pins both `pigeon` and `pigeon-app` tags, changed together when a change
  spans both (the 1.2 rename is one such change).
- The iOS app has no chunks; its builds (`yyc-ios` workflow, `YYC_SITE` secret) are unchanged apart from step 5's
  contract check and step 9's dispatch (A4.F56).

### 17.2 Manifests (`YYC/yyc-ai/deploy/pigeon.yaml`, applied by Flux)

The manifest is platform 9.4's (namespace `yyc-ai`), with this spec's values: Deployment `pigeon` (11.3; no
`replicas` field, the HPA owns it), container `pigeon` (`args: [serve]`, ports 8080/9090, env from Secret `pigeon`,
read-only root, uid 65532, no privilege escalation, all capabilities dropped, resources of 13),
`terminationGracePeriodSeconds: 320` (11.2), `preStop` sleep 5, initContainer `app`; Service `pigeon` port 80 -> 8080
(cloudflared's ingress target `http://pigeon.yyc-ai.svc.cluster.local:80`); HPA 2-6 at CPU 70 % with the slow
scale-down of platform 9.4; PDB `minAvailable: 1`.

NetworkPolicies (platform 10.2): ingress to 8080 only from the `cloudflared` pods of namespace `yyc-ai`; 9090 from
the node (kubelet probes) and the metrics scraper; egress to the `magpie`, `hermit` and `egress` (`model`) Services,
DNS, and TCP 443 to the internet (Cloudflare Access JWKS and get-identity sit behind Cloudflare anycast, so 443
anywhere is the practical rule). magpie, hermit and egress each admit pigeon in their own ingress policies.

### 17.3 Secrets

Sealed Secret `yyc-ai/pigeon`, from the `infra/secrets.txt` line of platform 12.2: `PIGEON_TOKEN_MAGPIE`,
`PIGEON_TOKEN_HERMIT`, `PIGEON_TOKEN_MODEL` from the pair-token store keys `YYC_TOKEN_PIGEON_{MAGPIE,HERMIT,MODEL}`
(also sealed into magpie's, hermit's and egress's Secrets as `MAGPIE_ACCEPT_PIGEON`, `HERMIT_ACCEPT_PIGEON`,
`EGRESS_ACCEPT_PIGEON`), and `PIGEON_VIEW_AS_KEYS`, `PIGEON_BLOB_KEYS` from `YYC_PIGEON_VIEW_AS_KEY[_PREV]`,
`YYC_PIGEON_BLOB_KEY[_PREV]`. Each value is 32 random bytes base64url, written with `scripts/store-put.py yyc
KEY=file:PATH --explain …` (platform 12.3). Bumping `yyc/secrets-rev` rolls the pods as today.

### 17.4 Images and CI (DE0CH/aviary)

pigeon is built, tested and published by aviary's single `ci.yml` (platform 6.1, R17): lint, the `test` job (pigeon
18.1-18.2, including the Node Paseo contract harness in `testing/paseo-contract`), the `e2e` harness (18.3), then
`publish` pushes `ghcr.io/de0ch/pigeon:<sha12>` and adds `pigeon-frames.ndjson` and `paseo-contract.tgz` (18.1) to
the `aviary-<sha12>` release. `pigeon` and `pigeon-app` are made public on first push (platform 5.5).

### 17.5 Cloudflare (tofu, `news-stream/deploy/cloudflare/tofu`)

The names and the change timing are platform 14's and cutover 9's; pigeon references them as they stand there:
tunnel resource `cloudflare_zero_trust_tunnel_cloudflared.pigeon` (tunnel name `pigeon`), output
`pigeon_tunnel_token`, store key `YYC_CF_PIGEON_TUNNEL_TOKEN` sealed into `yyc-ai/tunnel` (key `token`), the
signed-in path list `local.staff_ai_paths`, both ingress rules to `http://pigeon.yyc-ai.svc.cluster.local:80`, the
access-required rule `^/(${join("|", local.staff_ai_paths)})(/.*)?$`.

A test in aviary, `crates/pigeon/tests/signed_in_paths.rs`, runs in CI against a checkout of DE0CH/YYC (read-only,
the YYC commit named in the test's fixture, bumped with each YYC switch commit) and parses `local.staff_ai_paths`
from the tofu files:
- every pigeon signed-in prefix (`api`, `paseo-ws`, `ops`) must be in the list (a prefix pigeon treats as signed in
  but the edge leaves open would skip the Access login redirect);
- any extra entry must be one of the retired `ws`, `__jarvis`, and only while the old tunnel resource
  (`cloudflare_zero_trust_tunnel_cloudflared.yyc_ai`) still exists; after teardown the lists must be equal.
Today the two are kept in step by hand (gap-1 5.2). The 301 for `ai.yyc.deyaochen.com` stays in `yyc-edge`
(A19.F20); pigeon keeps the same path layout so deep links survive. `/yyc-logo.png` stays public (5.2.1).

---

## 18. Tests (the acceptance bar)

### 18.1 Contract suite (CI, every push)

pigeon runs against the in-process fakes of `testing/fakes` (`fake-magpie` and `fake-hermit`, implementing the
`magpie-api`/`hermit-api` server traits from fixtures) and a local JWKS (the Access verifier takes its key source as
a trait; tests inject keys; production has only the HTTPS source, no switch). A Node harness
(`testing/paseo-contract`, platform 4.1) drives a socket with `paseo/packages/client` and validates EVERY frame
against `WSOutboundMessageSchema` and the response schemas (zero failures allowed).

Keeping pigeon's Rust types in step with the apps' zod schema (7,793 lines in `messages.ts`, about 430 message types
of which pigeon sends or reads a few dozen):
- **Clients.** `testing/paseo-contract/clients.txt` lists the claudecodeui commits of every client that may be in use
  (cutover 13.1): the live web bundle's commit and the source commit of each TestFlight build since 2026-10-22. The
  harness checks out each listed commit's `paseo/packages/protocol` and validates every frame against each. New
  commits are appended automatically (17.1 step 9); a commit leaves the file only when its clients are gone (1.2 step
  3, cutover 13.1).
- **Generated schema, checked types.** For the newest listed commit, the harness exports JSON Schema of the inbound
  and outbound message unions with zod 4's `z.toJSONSchema` into `testing/paseo-contract/schema/<commit>.json`
  (committed). pigeon's message types in `paseo::messages` are hand-written serde types (typify does poorly on
  unions this size), and a Rust test serialises every type's fixtures and validates them against that JSON Schema
  (`jsonschema` crate); another test deserialises the schema's examples into the Rust types. A new commit whose
  schema changes a message pigeon handles therefore breaks the build until the Rust types match; a change to a
  message pigeon does not use passes.
- **Frame corpus.** The harness writes every frame it saw to `pigeon-frames.ndjson`, published with each aviary
  release (17.4), and the app's own workflows validate it against a new app commit before publishing (17.1 step 5).
  So drift is caught from both sides: pigeon cannot ship a frame an installed client rejects, and the apps cannot
  ship a schema that rejects a frame pigeon sends.

Cases, ported from `paseo-protocol.test.ts`, `paseo-timeline.test.ts`, `paseo-view-as.test.ts` (A2.F41, A20.F7) plus
the review additions:
- hello -> server_info valid; `protocolVersion: 2` -> hello.rejected; ping -> top-level pong before and after hello;
- every 7.5.1 stub valid; unknown request -> rpc_error unsupported; no requestId -> nothing;
- list snapshot paging (> 200 conversations), empty list -> emptyProjects;
- timeline tail/before/after, stale epoch -> reset, prompts, not-found shapes;
- rows of every kind project and validate (user with images and files, tool call lifecycle across two seqs, failed
  tool, SendUserFile metadata, `model.*` tagged assistant rows in both locales, one fixture per 7.9 code);
- MCP tool names: a resumed imported conversation whose history has `mcp__yyc__search` calls and whose new turn calls
  `mcp__tools__search` renders each with its own full name, as the old bridge did (the bridge-log parity test);
- live: rows stream with dense seq; a socket joining mid-turn catches up by gap; turn_completed after the last row;
  list upserts/removes; sandbox status to every socket and replayed on connect;
- the app's real reducer: the harness feeds pigeon's frames through `paseo/packages/app/src/timeline`
  (`processAgentStreamEvent` and the fetch paths) and asserts the rendered rows equal the rows of an uninterrupted
  run: zero catch-up RPCs on a conversation whose events interleave `title.changed`, `task.status`, `sandbox.*` and
  `ask.*` with rows (those take no row seq, 14.1.18), and no duplicate or lost row when the socket is closed mid-turn
  across a history rewrite (`retract` -> new epoch -> `agent.timeline.replacement` -> reset);
- send: accepted, busy (409), idempotent repeat, deleted conversation, images uploaded first;
- migration (7.10): `conversation_migrating` twice then 202 -> accepted, with the same `clientMessageId` on every
  attempt; still 409 after 45 s -> the "being moved" sentence in both locales, answered before the app's 60 s;
  cancel refused -> the "still finishing" sentence; view-as rename refused before any magpie call;
- cancel: during a running turn, during a hold (`turnId: null`), with nothing running; never an error row;
- create: updates + response; repeat key on a new socket returns the same conversation; creation.subscribe;
- rename/delete/delete-for-good/restore replies; asks relay and answer;
- view-as on the socket: rename/delete/restore/answer refused (no upstream call), send/create/cancel carry the
  viewer, `view_as_not_recorded` surfaces.

### 18.2 HTTP tests (ported from `jarvis/test/router.test.mjs`, A20.F0)

Access: 401 for missing JWT, wrong aud, wrong key, expired, nbf in the future, service token, unknown iss; the
`cf-access-token` header works; two teams each verified against their own JWKS; unknown kid refetch cooldown. People:
403 for not allowed, re-read after the cache, stale-on-error, offline closes sockets. Shell: hashed asset immutable,
html no-store, signed-out meta, signed-in no meta, product name in title, `/sw.js` signed out, missing chunk 404
no-store, `/yyc-logo.png` public. SPA fallback: `/new`, `/h/srv_conversations`,
`/h/srv_conversations/workspace/wks_<uuid>`, `/s/<22-char id>`, `/sessions` and an unknown page path each get
`index.html` signed out and signed in, with and without a query string, never a 404 or a redirect. Early message box:
the served `index.html` equals the bundle's byte for byte except the `<title>`, the app-title meta and (signed out)
the signed-out meta before `</head>`; the frame's `<textarea>` and its inline script are present and precede every
`<script src>`. `server: pigeon/<sha12>` on every response. Sign-in: `next` same-origin only, bare `/api` -> `/`, app-signin redirect/state
validation/404 without scheme. View-as: admin only, same-origin start, target must be allowed, recorded first (else
502), token by query reaches the person's data with viewer headers, refused for non-admin / other admin / forged /
expired / admin removed / person removed (403 VIEW_AS_REFUSED), REST writes refused except warm and uploads, `/ops`
refused in view-as. Ops: admin only, GET/HEAD only, dot-segments 404, forwarded with bearer and person, 502 page.
Uploads: streaming, 413 at both guards, one file only, localized errors. Files/blobs: passthrough, Range, bad
signature 404. CSRF: cross-origin POST refused, native (no Origin) allowed. `/api/unknown` 404 JSON after identity.

### 18.3 Drain and rollout tests

- Unit: SIGTERM -> readyz 503, listener closed, sockets closed with 1012 within 15 s, in-flight requests complete;
  exit within 20 s when nothing is transferring.
- Upload across a drain: a 50 MiB upload throttled to 2 Mbit/s starts, SIGTERM lands at 10 s; the upload completes
  (201 with the attachment), the process exits after it, within 320 s. The same for a 40 MiB image send arriving as
  one WebSocket message: the 1012 close comes only after the send's reply.
- Integration (the aviary harness, platform 7.8 E7, E9, E18, E20): two pigeons + magpie streaming a long turn; roll
  the Deployment; the client (the Node harness) must end with every row exactly once and a `turn_completed`.
- Live check after the cut (manual run, recorded in the task record): `kubectl rollout restart deploy/pigeon` during a
  real turn from a browser; the reply completes with no visible loss.

### 18.4 Live acceptance (ported operator scripts, A20.F17, A20.F18, A20.F20, A20.F21)

- `app-signin-check.py`: unchanged against the new host path (Access emailed code as bot@deyaochen.com), then
  `GET /api/jarvis/me` with the token as header and as cookie.
- `view-as-check` rewritten as `pigeon-view-as-check`: it runs in CI's integration stage against a real pigeon
  build with fake-hermit naming a throwaway Access-shaped issuer (local key pair) and a real magpie in a test
  namespace; it repeats today's checks (staff cannot start a view or use the admin's token; the admin's view reaches
  the person's data; REST writes refused; on the socket a rename is refused, a new conversation is recorded in
  magpie before its first message and the reply arrives) and cleans up its conversations. The browser test
  (`view-as-browser-test.mjs`) is re-pointed at the live host and run by hand with Deyao's admin session when a
  view-as change ships.
- `ui-pod-chat` becomes `magpie-chat` (magpie internal REST, magpie spec) plus a `paseo-chat-check.mjs` that speaks the
  Paseo protocol to the live host with bot@deyaochen.com's token: create, stream to `turn_completed`, upload a file,
  fetch history, share, delete for good.
- `load-timing.mjs` and `typable-first-test.mjs` against the live host: no regression from the baselines in A20.F22
  (signed-out Slow 4G usable ≤ 20 s; signed-in typable ≤ 0.7 s, send ≤ 12.6 s); pigeon serves `.br`, so timings should
  match the edge-compressed numbers.

---

## 19. Observability

- Logs: JSON lines `{t, level, msg, req?, conn?, person?: userKey, conv?, route?, status?, ms?}`; `userKey` =
  `u` + first 24 hex of sha256(email) (as today, so log searches carry over). Never message text, file names, tokens,
  cookies, view-as tokens or image data. Info: connection open/close (with clientType, appVersion, close code),
  view-as start, drains. Warn: refusals of admin-only routes, view-as refusals, slow-socket closes, upstream errors.
- Metrics on `:9090/metrics`: `pigeon_sockets{viewas}`, `pigeon_socket_closes_total{code}`,
  `pigeon_rpc_seconds{type}` (histogram), `pigeon_rpc_errors_total{type,code}`, `pigeon_pollers{kind}`,
  `pigeon_poll_seconds{kind}`, `pigeon_upstream_seconds{service,route,status}`, `pigeon_frames_out_total{type}`,
  `pigeon_frames_dropped_total{reason}`, `pigeon_http_requests_total{route,status}`, `pigeon_upload_bytes_total`,
  `pigeon_identity_total{result}`, `pigeon_hermit_stale_answers_total`, `pigeon_hello_total{protocol,clientType}`
  (decides when protocol 1 can go, 1.2), `pigeon_migration_retries_total{action,outcome}` (7.10),
  `pigeon_drain_transfer_seconds` (11.2).
- hermit's ops page shows pigeon's Deployment and pods as it shows `ui` today (hermit reads the k8s API).

---

## 20. Cut-over (pigeon's part)

The switch is cutover spec sections 9-12 (its own tunnel `pigeon` on the test host `pigeon-test.yyc-internal.com`
first, then the one-line DNS flip of `origin_ai`, the old connectors' drain, the importer's handover). pigeon's
obligations in it, all specified above:
- Deployed in P1 beside `ui`, dark, with the switch window code (`crates/pigeon/src/legacy/`: LU, tickets, the 409
  `route: "legacy"` re-route, the T0→gate upload tee, copy-at-send under the ticket, the legacy view-as key;
  DESIGN 9.3-9.5) already in, magpie's window release first (cutover 13.3). There is no 7.10 retry and no
  "being moved" sentence (DESIGN 12 D14).
- Serves the first `pigeon-app` bundle seeded with the live `ui` image's chunks (17.1 step 6), so tabs that load
  from `ui` and reconnect to pigeon keep working.
- Validates frames against the `ui` build's protocol commit too (`clients.txt`, 18.1).
- Answers every reconnecting tab with a list snapshot and no timeline reset: a moved tab's cursors stay valid
  because LU serves a legacy conversation from the same old child; the one `agent.timeline.replacement` comes at the
  conversation's release (DESIGN 9.3).
- Accepts view-as tokens of the old router through the legacy key while any conversation is legacy (6.2); refuses
  the `/ws` socket (5.1 row 22).
- Serves `GET /internal/switch/activity` (per person: last warm call and last upload) for the runner's move, and on an
  LU 401 answers with its signed-out contract, never a write error (DESIGN 9.3, 9.5).
- Carries `server: pigeon/<sha12>` (5.13) for the switch runner's probe.
- After the switch, the 1.2 constants stay until their rename; `migrating` handling (7.10) goes in the release that
  follows the end of the import, before magpie drops the column.

The old `ui` Deployment, Services `ui`/`ui-peers`, its NetworkPolicies, image pin, env and secrets, CloudCLI's
`jarvis-image.yml` and router code, and the `cloudcli` database go in cutover P5 (cutover 15).

---

## 21. Coverage: every inventory feature with a pigeon target

Generated from `inventory.json` (every feature whose `target_home` mentions pigeon, 162 rows), plus the 17 app-side
features of areas A4 and A17 whose target is "apps" but which depend on what pigeon or its build does (table at the
end of the section). "Where" points at this spec's section; "Outcome" says what happens to the feature.

| ID | Feature | Inventory target | Where | Outcome |
|---|---|---|---|---|
| A0.F0 | Cloudflare Access JWT verification | pigeon | 4.2-4.3 | kept: same checks; also reads cf-access-token |
| A0.F1 | Multiple Access teams (access.also) | pigeon | 4.1, 4.3 | kept as hermit's signIn.access list (no access.also setting) |
| A0.F2 | users / @domain allowlist | hermit (who may use it), checked by pigeon | 4.4 | moved: hermit answers allowed; pigeon checks per request, 15 s cache |
| A0.F3 | admins | hermit (roles), checked by pigeon | 4.4, 5.11, 6.1 | moved: role from hermit, re-read live |
| A0.F5 | Signed-out public shell | pigeon (or apps static bundle served by pigeon) | 5.2.2 | kept: shell from the mounted bundle, product name from hermit |
| A0.F6 | Note: signed-in visitors also get the app's HTML/JS from CloudCLI, not from the shell | pigeon (serve static without a backend) | 5.2.1 | changed: signed-in assets served statelessly, no per-person process |
| A0.F7 | GET /api/jarvis/signin?next= | pigeon | 5.3.1 | kept |
| A0.F8 | Bare signed-in prefix redirect | pigeon | 5.1 row 1 | kept for /api, /api/, /paseo-ws; /ws dropped |
| A0.F9 | GET /api/jarvis/me | pigeon | 5.4, 4.5 | kept; sharing:true added |
| A0.F10 | GET /api/jarvis/app-signin?state= (iOS native sign-in) | pigeon | 5.3.3 | kept; scheme from hermit appScheme |
| A0.F11 | Operator page forwarding /ops/* | pigeon (forward to whichever service owns ops data; hermit for sandboxes) | 5.11 | kept; forwarded to hermit /ops |
| A0.F12 | View-as start: POST /api/jarvis/view-as | pigeon (token) + magpie/hermit (audit record) | 6.1 | kept; recorded in magpie POST /v1/view-as |
| A0.F13 | View-as token format and checks | pigeon | 6.2 | kept format; own key list; query param only |
| A0.F14 | View-as read-only enforcement | pigeon (method gate) + magpie (record before send, refuse renames/deletes) | 6.3, 6.4 | kept; magpie records and refuses too |
| A0.F24 | Rendezvous hashing across replicas | drop (stateless pigeon + magpie need no affinity) — but magpie needs a single-writer story for li... | 10 | dropped: no rendezvous, no affinity |
| A0.F26 | HTTP proxy to the local process | pigeon (routing REST to magpie/hermit) | 5.1-5.12 | replaced by an allow-list of routes forwarded to magpie/hermit/model |
| A0.F28 | WebSocket proxying (/paseo-ws, /ws, any upgrade) | pigeon (Paseo protocol to apps) | 7.1, 5.1 row 22 | replaced: pigeon speaks Paseo itself; only /paseo-ws upgrades |
| A0.F30 | Graceful drain on SIGTERM / SIGINT | pigeon/magpie/supervisor each need their own drain; the 'keep a running turn alive across a rollo... | 11.2 | replaced: short socket drain; turns not tied to pigeon |
| A0.F31 | Rollout shape (zero downtime) | pigeon/magpie deployments | 11.3 | kept shape; no JuiceFS node affinity |
| A0.F34 | POST /api/jarvis/warm (passed through) | pigeon → hermit | 5.6 | kept: 202 at once, coalesced, hermit /v1/warm |
| A0.F39 | Edge: Access app paths and tunnel ingress | pigeon (+ tofu) | 17.5 | changed: paths api, paseo-ws, ops; test checks tofu vs pigeon |
| A0.F40 | NetworkPolicies around ui | pigeon/magpie/hermit policies | 17.2 | replaced by pigeon-ingress/egress policies; no sandbox overlay access |
| A1.F0 | Jarvis mode switch and the two modes | drop (with the rewrite, a deployment-specific switch goes away; the behaviour splits into pigeon/... | 1.1 | dropped: no mode switch |
| A1.F1 | Instance config read (product name, locale, model, label, title model) | magpie (product, locale, title model) / pigeon (product name in the page) — served over REST from... | 4.1, 5.5 | product/locale from hermit, model from model endpoint; no config file |
| A1.F4 | Request gate: X-Jarvis-User must equal the owner | pigeon (sign-in, identity propagation); magpie/hermit verify the caller service + user | 10 | replaced: Aviary-Person with pigeon's bearer; magpie/hermit verify |
| A1.F5 | Routes hidden in Jarvis mode (404 NOT_AVAILABLE) | pigeon (only expose what staff use) | 5.12, 15 | replaced by allow-list; everything else 404 NOT_AVAILABLE |
| A1.F6 | View as (admin acting as staff) in this process | pigeon (view-as token + forwarding) / magpie (refuse mutations, record sends) / web ops page data | 6 | pigeon token + gates; magpie records and refuses |
| A1.F24 | Readable errors shown to staff (zh-HK / en) | magpie (rendering of events) / pigeon (localized strings) | 7.9, 7.8 | magpie classifies (incl. API Error text into model.* tags); pigeon localizes the codes |
| A1.F32 | Shared page view /s/<id> and its file downloads | magpie (share data) / pigeon (route /s/<id>) | 5.10 | pigeon routes to magpie shares; rows projected by pigeon |
| A1.F39 | session_upserted / session_removed broadcasts and cross-replica NOTIFY relay | magpie (event feed) → pigeon (push to apps) | 8.1, 7.6.4 | replaced by magpie change feed (long-poll) -> list pushes |
| A1.F45 | Typing warms a sandbox host | hermit (demand hint), routed by pigeon | 5.6 | kept |
| A1.F46 | Product name in the served page | pigeon | 5.2.2 | kept |
| A1.F47 | Health endpoint and router-driven process lifecycle | supervisor (turn survives restarts) / pigeon+magpie stateless rolling deploys | 11 | dropped per-process lifecycle; pigeon probes on :9090 |
| A2.F0 | Socket route /paseo-ws | pigeon | 7.1.1 | kept |
| A2.F1 | Server-side WS heartbeat | pigeon | 7.1.3 | kept (30 s ping) |
| A2.F2 | Application ping/pong | pigeon | 7.1.3 | kept |
| A2.F3 | hello -> server_info | pigeon | 7.2 | kept; version echo; protocol gate added |
| A2.F4 | Envelope and drop rules | pigeon | 7.3 | kept; handlers never leave a request unanswered |
| A2.F5 | No-op client messages | pigeon | 7.5.1 | kept |
| A2.F6 | subscription.release.request | pigeon | 7.5.1 | changed: release now clears the named subscription |
| A2.F7 | session.events.set_subscription.request | pigeon | 7.5.1 | kept |
| A2.F8 | workspace.label.list.request | pigeon | 7.5.1 | kept; stable generation |
| A2.F9 | hosts.list.request | pigeon | 7.5.1 | kept |
| A2.F10 | Developer-machine stubs | pigeon | 7.5.1 | kept (same payloads) |
| A2.F11 | Unsupported client requests (rpc_error 'unsupported') | pigeon | 7.5.10 | kept |
| A2.F12 | get_providers_snapshot_request (providers/models) | pigeon (model from magpie/hermit config via REST) | 7.5.1, 5.5 | kept shape; model from model endpoint |
| A2.F13 | project.list.request | pigeon | 7.5.1 | kept |
| A2.F14 | fetch_workspaces_request (conversation list) | pigeon (shape) over magpie (list) | 7.5.2 | kept shape over magpie M1; now paged |
| A2.F15 | fetch_agents_request (agent list) | pigeon over magpie | 7.5.2, 7.7 | kept shape over magpie M1 |
| A2.F16 | fetch_agent_request | pigeon over magpie | 7.5.2 | kept over M2 |
| A2.F17 | fetch_agent_history_request (Recently deleted / History page) | magpie (deletions), pigeon (shape) | 7.5.2 | kept shape; paged, one query (no N+1) |
| A2.F18 | agent.timeline.set_subscription.request | pigeon | 7.5.3, 8.2 | kept; drives timeline pollers |
| A2.F19 | fetch_agent_timeline_request (timeline paging, epochs, seq) | magpie (events paged by seq) + pigeon (projection) | 7.5.3 | kept over magpie M9; stable epochs |
| A2.F20 | Timeline row shape and projection | magpie (event rows) / pigeon (projection to Paseo items) | 7.8, 14.1.18 | split: magpie owns the row model (pairing, dense row seq, epoch), pigeon pure projection incl. tool mapper port |
| A2.F22 | agent.timeline.list_prompts.request (prompt index) | magpie/pigeon | 7.5.3 | kept over M10 |
| A2.F23 | send_agent_message_request (send) | pigeon (protocol) -> magpie (append user event, blobs) -> hermit/supervisor (run) | 7.5.4 | pigeon -> magpie M7 (images as uploads), atomic admission in magpie |
| A2.F24 | cancel_agent_request (stop) | pigeon -> supervisor | 7.5.6 | pigeon -> magpie M8; recorded in view-as |
| A2.F25 | workspace.create.request (new conversation + first message) | pigeon -> magpie (create conversation) -> hermit | 7.5.5 | pigeon -> magpie M3; durable idempotency; always answers |
| A2.F29 | Live list updates: agent_update / workspace_update | pigeon (fan-out) fed by magpie list events | 7.6.4 | from magpie change feed; one seq per change |
| A2.F30 | Live timeline stream: agent_stream timeline events | pigeon (stream) from magpie events | 7.6.1, 8 | from magpie timeline long-poll |
| A2.F31 | Turn lifecycle: turn_started / turn_completed / turn_failed / turn_canceled | supervisor (run state) -> magpie events -> pigeon | 7.6.2, 8.4 | from magpie turn changes, ordered after rows |
| A2.F32 | yyc.sandbox_status (preparing chip) — protocol addition | hermit (state) -> pigeon | 7.6.5 | from magpie sandbox changes; replayed on connect |
| A2.F36 | View-as on the Paseo socket | pigeon (viewer identity) + hermit/web ops log | 6.4 | kept; cancel now recorded |
| A2.F39 | REST surface the app uses beside the socket | pigeon (gateway) -> magpie/hermit | 5.1, 15 | each route given a home |
| A2.F40 | Fixed zh-HK strings and defaults in the bridge | pigeon | 7.9 | kept, both locales |
| A2.F41 | Bridge tests (contract checks) | pigeon (contract test suite) | 18.1 | ported to the Rust pigeon + Node schema validator |
| A3.F4 | POST /internal/cc/view-as: audit record of an admin viewing or sending as a staff member | pigeon enforces; the audit rows belong with magpie (conversation events) or a shared audit table | 6.1, 6.5 | pigeon enforces; audit in magpie (M17 and in-transaction records) |
| A3.F47 | Ops section: view-as log | magpie/pigeon audit | 6.5, 5.11 | rows in magpie, shown by hermit's ops page |
| A3.F48 | ops_events recording | hermit (starts), magpie/pigeon (view_as) | 6.5 | view_as rows move to magpie (39 imported); sandbox_start stays hermit |
| A4.F0 | WebSocket transport and envelope | pigeon | 7.1, 7.3 | kept |
| A4.F1 | Connect and server_info handshake | pigeon | 7.2 | kept |
| A4.F2 | Reconnect with backoff | pigeon | 9 | kept (client); pigeon needs nothing |
| A4.F3 | Liveness heartbeat (ping/pong) | pigeon | 7.1.3 | kept |
| A4.F4 | client_heartbeat (presence) | pigeon | 7.5.1 | ignored, not routed |
| A4.F5 | Send queue while connecting | pigeon | 9 | client behaviour; RPCs answer < 60 s |
| A4.F6 | Owned subscriptions (re-subscribe on reconnect) | pigeon | 9, 7.5 | kept; subscription ids per socket |
| A4.F9 | Project list and events subscription | pigeon | 7.5.1 | kept |
| A4.F17 | Send a message | pigeon | 7.5.4 | kept |
| A4.F19 | Stop a reply | pigeon | 7.5.6 | kept |
| A4.F27 | Provider/model snapshot (single fixed model) | pigeon | 7.5.1 | kept |
| A4.F28 | Read-only model label in composer | pigeon | 5.5 | kept |
| A4.F29 | Stubbed developer messages the app still sends | pigeon | 7.5.1 | kept |
| A4.F30 | server_info features/capabilities the app gates on | pigeon | 7.2 | kept |
| A4.F42 | Who am I / greeting | pigeon | 5.4 | kept |
| A4.F43 | Web sign-in / signed-out shell | pigeon | 5.2.2, 5.3.1 | kept |
| A4.F44 | Sign out | pigeon | 5.3.2 | unchanged (edge logout / Keychain) |
| A4.F45 | iOS Access sign-in and token | pigeon | 5.3.3, 4.2 | kept; header now honoured directly |
| A4.F46 | View as (admin impersonation, read-only by default) | pigeon | 6 | kept (client unchanged) |
| A4.F53 | Branding / product name | pigeon | 5.2.2, 7.2 | product name from hermit |
| A4.F59 | Unsupported RPC error surface | pigeon | 7.3 | kept |
| A7.F4 | config.json: product, publicUrl, access | pigeon (sign-in), magpie/hermit (locale for messages) | 4.1 | moved to hermit /deployment; no config.json in pigeon |
| A7.F5 | config.json: users / admins | hermit (who may use) + pigeon (sign-in) | 4.4 | hermit owns the list; pigeon asks |
| A7.F12 | ui Deployment rollout (surge-first, topology spread, drain) | pigeon (stateless gateway drain) + supervisor (runs survive restarts without a chat-drain wait) | 11 | replaced by short drain; turns survive in supervisor |
| A7.F13 | ui per-user process placement (rendezvous hashing + peer holds) | drop (pigeon is stateless; magpie/supervisor own the conversation state) | 10 | dropped |
| A7.F16 | Signed-out shell and sign-in paths | pigeon | 5.1, 5.2, 17.5 | kept; /ws and /__jarvis leave the signed-in paths |
| A7.F17 | iOS app sign-in callback | pigeon | 5.3.3 | kept; scheme from hermit |
| A7.F19 | View as (admin impersonation, read-mostly) | pigeon (token) + magpie (record of sends) | 6 | kept |
| A7.F26 | cloudflared connector | pigeon front door (ops) | 17.2, 20 | cloudflared unchanged; ingress service -> pigeon |
| A7.F37 | Operator page /ops (data sources) | hermit + magpie (data) / pigeon (page delivery) | 5.11 | page delivery = pigeon forwarding; data = hermit aggregating |
| A7.F42 | Cloudflare Access app, policies and login design | pigeon (front door) | 17.5, 5.2.1 | unchanged at Cloudflare; logo stays public |
| A7.F43 | Tunnel yyc-ai ingress, DNS and custom hostnames | pigeon | 17.5, 20 | ingress points at pigeon; paths shrink |
| A7.F48 | claudecodeui jarvis-image workflow (UI image) | pigeon/magpie images | 17.1, 17.4 | replaced: pigeon image (aviary) + pigeon-app bundle image (fork) |
| A8.F22 | History reading for the UI and the Paseo apps | magpie (history pages straight from rows by seq; pigeon relays them) | 7.5.3 | magpie pages rows by seq; pigeon relays |
| A8.F29 | CloudCLI readers of user-level Claude config (not used by YYC today) | drop (or pigeon/magpie only if a feature needs it) | 15 | dropped |
| A9.F7 | New send supersedes a hold (releaseHeldSession and the addSession supersede) | supervisor (one owner per sandbox makes supersede a local decision); hermit/pigeon must decide wh... | 7.5.4 | no confirmation; magpie/supervisor admit and supersede |
| A9.F8 | Abort and stop-task during a hold | pigeon (message) → supervisor (stop) | 7.5.6 | cancel always forwarded; magpie stops held tasks; no error row |
| A9.F12 | Paseo hub/timeline during a hold | pigeon (protocol) + magpie (timeline/events with explicit background-turn boundaries) | 7.6.2, 7.5.6 | explicit boundaries for agent-started turns; cancel in a hold works |
| A9.F14 | Live task_status frames | supervisor emits; magpie stores; pigeon maps to Paseo | 7.8 | not shown (no Paseo task UI); magpie stores, running state via turn |
| A9.F15 | Router idle sweep, LRU eviction and drain vs holds | pigeon is stateless in the target; supervisor must own the process so gateway rollouts never touc... | 11 | dropped: nothing per person in pigeon |
| A10.F0 | jarvisApiGuard: owner check + UNAVAILABLE 404 list | drop (pigeon authenticates once and only exposes the routes the target design defines; the deny-l... | 5.12 | dropped; allow-list |
| A10.F1 | Platform-mode auth (first DB user) | drop (identity comes from pigeon's sign-in; magpie keys by person, no users table) | 4, 16 | dropped; identity = Access email |
| A10.F8 | /api/settings notification-preferences + push (vapid-public-key, subscribe, unsubscribe) | drop (no caller; Paseo has no web push and the iOS app has no push notifications, docs/paseo-ui.m... | 15 | dropped |
| A10.F18 | GET /api/providers/claude/models | pigeon (or magpie) as a tiny instance-info route: KEEP | 5.5 | kept as a pigeon route over model |
| A10.F24 | SPA catch-all + static public/ and dist/ | pigeon (serve the app and product-name title statelessly; keep serving /sw.js kill-switch for old... | 5.2 | kept statelessly; sw.js also signed out; api-docs dropped |
| A10.F25 | GET /health on each child | supervisor/pigeon readiness probes | 11.1 | pigeon :9090 probes |
| A10.F28 | CORS and JSON limits | pigeon (explicit same-origin policy and per-route limits) | 5.13 | explicit same-origin and per-route limits |
| A11.F8 | stdout control_request can_use_tool → canUseTool → permission_request | supervisor (holds the pending control_request); pigeon (relays agent_permission_request/response ... | 7.5.9 | relayed when magpie reports an ask |
| A11.F9 | stdout control_request hook_callback (Notification hook) | supervisor (or drop and derive the notification from the permission event in magpie/pigeon) | 7.5.9 | dropped on pigeon side (no notification) |
| A11.F11 | AskUserQuestion / ExitPlanMode in YYC (bypassPermissions) | pigeon (map to agent_permission_request/response) + supervisor (hold the ask) + magpie (persist t... | 7.5.9 | generic relay; tool policy is supervisor's |
| A12.F4 | users table (the one owner row) | magpie (a person record keyed by email/userKey) or pigeon (identity); username = email is the joi... | 16 | dropped |
| A12.F5 | app_config (key/value, JWT secret) | drop (nothing to migrate; pigeon holds signing keys from config/secrets) | 16 | dropped |
| A12.F14 | vapid_keys (Web Push key pair per schema) | pigeon or magpie: one instance-wide key from a secret; nothing needs carrying since push_subscrip... | 16 | dropped |
| A12.F15 | push_subscriptions | pigeon or magpie (only if web push is kept); nothing to migrate | 16 | dropped |
| A13.F10 | ops_events view_as (current, never pruned) | pigeon/magpie audit (view-as is an access audit) | 6.5 | imported into magpie |
| A14.F0 | Hub chat client bootstrapped from the first Paseo connection | magpie (conversation event fan-out) / pigeon (protocol); drop the fake-socket design: talk to sup... | 8 | dropped: pollers over REST instead of a fake chat socket |
| A14.F1 | Caller check at upgrade only (X-Jarvis-User) | pigeon (sign-in) + hermit (who may use a sandbox) | 10, 7.1.4 | replaced: per-request identity, sockets re-checked every 60 s |
| A14.F2 | View-as audit and limits on the Paseo socket | pigeon (carries viewer) + magpie (records the send with the viewer on the event row) | 6.4 | kept; viewer travels to magpie on each write |
| A14.F11 | Sandbox 'preparing' chip | hermit (sandbox lifecycle) -> pigeon | 7.6.5, 7.5.2 | kept; replayed on connect |
| A15.F0 | Hashed chunk naming and per-image dist (no previous-build chunks kept) | pigeon (serving the app shell/assets; the rewrite should keep N-1 builds' chunks or serve them fr... | 17.1 | fixed: bundle keeps recent builds' hashed assets |
| A15.F1 | Router signed-out static shell with cache headers | pigeon | 5.2.1 | kept headers; 404 no-store instead of 401 |
| A15.F2 | Signed-in static assets go through the per-person CloudCLI child (and may start it) | pigeon (assets should never need a per-user process) | 5.2.1 | fixed: assets never need identity |
| A15.F3 | Cloudflare edge caching of immutable assets | pigeon/ops (explicit cache rules; keep previous builds' assets at origin) | 5.2.1, 17.1 | 404 no-store; old chunks at origin |
| A15.F4 | Entry chunk failure: frozen boot frame | pigeon | 17.1 | mitigated: entry chunks of recent builds stay served |
| A15.F5 | Lazy chunk retry and one-reload-per-10-minutes | pigeon (client app behaviour to keep) | 17.1 | client logic kept as backstop |
| A15.F6 | What a failed area shows | pigeon (client) | 17.1 | client unchanged |
| A15.F7 | Staged loading: reply display, language, sidebar, workspace, hosts chunks | pigeon (client) | 17.1 | mitigated by kept chunks |
| A15.F8 | Server-message validator chunk without page reload | pigeon (client) | 17.1 | mitigated by kept chunks |
| A15.F9 | Signed-out shell vs signed-in page on different replicas/images during a rollout | pigeon (stateless; serve all assets itself, never from a per-user process) | 17.1, 10 | fixed: every replica serves every build's assets, no per-person path |
| A15.F10 | Old-CloudCLI service worker kill switch | pigeon | 5.2.1 | fixed: sw.js served signed out too |
| A15.F11 | Hello handshake: no version gate | pigeon (should own a min-version gate using hello.rejected incompatible_protocol) | 7.2 | gate on protocolVersion via hello.rejected |
| A15.F12 | appVersion sent is the npm package version, not the iOS build | pigeon | 7.2 | logged; real build id needs an app change later |
| A15.F13 | Client frame validation: silent drop vs request failure | pigeon (protocol compatibility policy) / magpie (row shapes) | 7, 18.1 | schema-exact frames; every frame validated in CI |
| A15.F14 | Server changes that would break shipped iOS apps | pigeon | 1.1, 7.4 | public paths, ids, headers kept exactly |
| A16.F0 | Survival: ui pod drain longer than JARVIS_UI_DRAIN_SECONDS (900 s) | supervisor (Claude Code runs detached and outlives the gateway) + magpie (events as rows paged by... | 9, 11.2 | fixed: turns outlive pigeon; resume by seq |
| A16.F2 | Survival: LRU eviction / idle stop | pigeon (stateless, no per-user processes to evict) / supervisor | 10 | dropped (no processes to evict) |
| A16.F3 | Survival: router peer hand-off or rendezvous change while a WebSocket is open | pigeon (stateless) + magpie (one owner of conversation events; any gateway can tail them by seq) | 9, 10 | fixed: no hand-off; any replica tails magpie |
| A16.F14 | Stop (chat.abort) during a dead /proc link | pigeon/magpie (Stop recorded as an event at once) + supervisor (kills locally) | 7.5.6 | cancel acknowledged at once; magpie ends the turn |
| A16.F16 | Readable error mapping shown to staff (zh-HK) | pigeon (presentation) with error codes from magpie/hermit | 7.9 | codes localized; new sandbox.lost |
| A17.F16 | Ops page 'people' incl. former users | hermit (data) / pigeon (ops page) | 6.1, 5.11 | hermit data; former users refused for view-as |
| A17.F18 | View-as and deletes | pigeon (guard) / magpie (enforce) | 6.4 | pigeon gate + magpie enforcement |
| A19.F4 | Snapshot endpoint auth and caching (producer side the consumer relies on) | hermit or pigeon-fronted ops endpoint; a separate read-only ops token would be cleaner than reusi... | 5.11 | hermit owns the snapshot auth; pigeon forwards with its own token |
| A19.F19 | Front door paths that need sign-in (Access app + tunnel ingress) | pigeon (must serve the same signed-in paths, the public shell and /yyc-logo.png; any new path nee... | 17.5, 5.2.1 | paths api/paseo-ws/ops; logo public; CI check against tofu |
| A19.F20 | Old hostname 301 redirect | stays in edge; pigeon must keep serving the old path layout (deep links redirect path-for-path) | 17.5 | stays at the edge; path layout unchanged |
| A20.F0 | Router test suite (Access, per-person process, rendezvous, rollouts, drain, view-as, ops proxy, sign-in, sent-file snapshot) | pigeon (Access, sign-in, view-as, ops proxy, drain and routing); sent-files snapshot -> magpie; t... | 18.2, 18.3 | ported (process/cap/idle cases dropped, rollout guarantee re-expressed) |
| A20.F7 | Paseo bridge tests | pigeon (Paseo protocol) + magpie (timeline rows paged by seq, epoch/reset, prompt index) | 18.1 | ported |
| A20.F8 | Paseo app YYC tests | apps (unchanged client; must keep passing against pigeon) | 18.1 | app tests unchanged; must pass against pigeon |
| A20.F17 | ui-pod-chat (real chat from inside a ui pod) | tools (operator check); must be rewritten against pigeon/magpie REST once the per-person CloudCLI... | 18.4 | replaced by magpie-chat + paseo-chat-check |
| A20.F18 | View-as checks (pod and browser) | pigeon (view-as token enforcement) + magpie (recording before send); acceptance tests to port | 18.4 | ported as pigeon-view-as-check + browser test |
| A20.F20 | App sign-in check | pigeon | 18.4 | kept |
| A20.F21 | Load timing and early-box tests (Chrome, Fast/Slow 4G) | apps + pigeon (pigeon will serve the shell/static and must send it compressed or sit behind the e... | 18.4, 5.2.1 | kept; pigeon serves precompressed assets |
| A20.F22 | Measured baselines (acceptance numbers) | hermit (sandbox start, warm, migration), magpie (title, share latency), pigeon/apps (load timings... | 18.4 | pigeon's load-timing baselines are the bar |
| A20.F23 | Zero-downtime rollout configuration verified by tests | pigeon/magpie/hermit each need the equivalent drain tests plus a real rollout check | 11.3, 18.3 | pigeon drain tests + real rollout check |

App-side features (target "apps"; the apps are unchanged, and pigeon relies on or provides the following):

| ID | Feature | Inventory target | Where | Outcome |
|---|---|---|---|---|
| A4.F13 | Local timeline replica cache | apps | 9, 14.1.18, 7.4 | apps unchanged; pigeon relies on magpie's epoch being stable across replicas, restarts and rollouts and changing only on a rebuild (then `reset`), on dense row seqs, and on `serverId`/ids never changing |
| A4.F18 | Queue while a reply runs | apps | 7.5.4, 8.4 | apps unchanged; pigeon delivers `turn_completed` only after the turn's last row (`timelineSeq`), so the queue drains after a synchronised timeline |
| A4.F21 | New-chat send-ready gate | apps | 7.2, 7.5.1, 7.5.2 | apps unchanged; pigeon answers hello, the list and the project list on connect without waiting on a sandbox |
| A4.F39 | Opening server files on iOS vs web | apps | 4.2, 5.8, 5.9 | apps unchanged; pigeon accepts the Access cookie (web) and the `cf-access-token` header (iOS downloads and previews) on file and blob routes |
| A4.F47 | Early message box before JS loads (web) | apps | 5.2.2, 18.2 | apps unchanged; pigeon serves `index.html` byte for byte except title and meta, with the signed-out meta before the box's inline script; tested |
| A4.F48 | Page frame and staged loading | apps | 5.2.1, 17.1 | apps unchanged; pigeon serves every recent build's hashed chunks, immutable, precompressed |
| A4.F49 | Offline / unreachable screen | apps | 5.2.1, 11.1 | apps unchanged; pigeon serves the shell without any upstream, so the screen appears only when pigeon itself is unreachable |
| A4.F50 | Routes / screens | apps | 5.2.2, 18.2 | apps unchanged; pigeon's SPA fallback serves `index.html` for `/`, `/new`, `/h/<host>/workspace/wks_<id>`, `/s/<id>` and every page path, signed in or not; tested |
| A4.F51 | Conversation menu | apps | 5.4, 7.5.7, 7.5.8, 5.10 | apps unchanged; `sharing: true` keeps Share in the menu; rename/delete/share as specified |
| A4.F52 | Localisation | apps | 4.1, 7.9 | apps unchanged; pigeon's server strings follow `product.locale` (zh for YYC) |
| A4.F53 | Branding / product name | pigeon | 5.2.2, 7.2 | product name from hermit (also listed above) |
| A4.F54 | Drafts persistence | apps | 7.5.5 | apps unchanged; magpie keys create idempotency on the draftId (`idempotencyKey`) and sends on `clientMessageId` |
| A4.F55 | Last conversation memory | apps | 7.4 | apps unchanged; `wks_<id>` and conversation ids are stable (imported ids kept) |
| A4.F56 | iOS build and TestFlight CI | apps | 17.1 | unchanged, plus the frame-corpus check and the `clients.txt` dispatch per TestFlight upload |
| A4.F57 | Web build | apps | 17.1 | moves from `jarvis-image.yml` into `pigeon-app.yml`: 4-vCPU/16 GiB runner, 45 min timeout, 6 GiB Metro heap (`NODE_OPTIONS=--max-old-space-size=6144` via `build-paseo-ui.mjs`), no-`__common` patch applied and asserted, chunk budgets checked |
| A4.F58 | Developer local mode | apps | 5.13 | dropped (cross-origin by design); the dev path is a local harness pigeon serving a local export same-origin; the `EXPO_PUBLIC_LOCAL_DAEMON` branch is deleted |
| A17.F21 | Client replica caches | apps | 7.6.4, 8.2, 8.3, 9 | apps unchanged; an offline client gets `conversation_removed` effects on reconnect because the list comes back as a snapshot (the deleted conversation is absent) and the change feed resumes after it; a purged conversation's timeline fetch answers not-found, which drops its cached rows. Remote wipe of a device's cache stays out of scope (no server can reach an offline device) |

---

## 22. What the other specs must change to match this one

pigeon is built against these; each is a text change in the named spec, listed so its owner can apply it in one pass.

### 22.1 magpie (`spec-magpie.md`)

1. **Row model** (blocker). Adopt 14.1.18: a timeline rows projection beside the events, written in the event's
   transaction under the per-conversation lock, with its own dense row seq per (conversation, epoch), tool rows
   re-issued at the next row seq when the result or sent files land, a stable epoch changed only by a rebuild
   (`retract`, derivation version), and `timeline_replaced` on rebuild. Rewrite magpie 1 ("the projection of events
   into Paseo timeline rows: pigeon") and 7 accordingly: magpie owns rows, pigeon only maps one row to one Paseo row.
   The raw event routes (`/v1/conversations/{id}/events`, `include=related`, `follow=1`) stay for admin, import
   verification and the supervisor's needs, not for pigeon; the NDJSON `follow` stream and `/v1/feed` have no pigeon
   consumer and are not built for it.
2. **16.1 = 14.1 here**, under `/v1`, camelCase: M1 (`state=active|deleted`, `search`, `list: {generation,
   headSeq}`), M4 `GET /v1/creations/{key}`, M5 `PATCH` (not `PUT …/title`), M6 `POST …/delete`, `POST …/restore`,
   `DELETE` = permanent with 409 `not_deleted` (not `?permanent=1`), M7 body `{clientMessageId, text, images:
   [{uploadId}], files: [{path,…}]}` (not `uploads[]`), M8 `POST …/cancel` (conversation-level; `stop_tasks` during a
   hold; never `no_active_turn`; replaces `…/turns/{turn}/stop`), M9 timeline with `waitMs` long-poll, M10 prompts in
   row seqs, M11 202, M12 `GET /v1/changes` long-poll with the entry types of 14.1 (`turn` with `timelineSeq`,
   `sandbox`, `ask`, `ask_resolved`, `timeline_replaced`, upserts and removes), M13 headers `Aviary-Upload-Name` /
   `Aviary-Upload-Kind` returning `{uploadId, path, name, size, mimeType}`, M15 `GET /v1/blobs/{blobId}?variant=`,
   M16 shares returning `rows` inline and `nothing_to_share` as 409, M17 `POST /v1/view-as`.
3. **Conversation object** as 14.1.0 (`titleSource`, `claudeSessionId`, `turn`, `lastTurn`, `sandbox`, `asks`,
   `migrating`; not `name_source`, `run{}`, `provider_session_id`, `head_seq`). One turn origin enum `user|cli|import`.
4. **Error codes** lower_snake_case and flat (`turn_running`, `conversation_deleted`, `conversation_migrating` with
   `retryAfterMs`, `view_as_not_recorded`, `view_as_read_only`, `empty_title`, `title_too_long`, `not_deleted`,
   `already_resolved`, `nothing_to_share`, `unknown_upload`, `cursor_expired`, `too_large`, `unsupported_type`,
   `bad_cursor`); data codes dotted as 7.9 lists, plus `run.oom` and `legacy.cut`.
5. **No live-frame route** in this release (8.7): no `POST /v1/sandbox/turns/{turn}/live`, no `live` entries, no
   `GET /v1/conversations/{id}/live`.
6. Model-error classification (`model.*` with `status` and `message`) lands on the assistant row's `error` (7.8).
7. View-as (magpie 13): keep the audit in the same transaction as the send, create and cancel it records (already
   magpie's design), under `Aviary-Viewer` / `Aviary-View-Session`; the start record is M17 `POST /v1/view-as`
   `{admin, person, session, expiresAt}` (not `/v1/audit/view-as` with `event: "start"`).

### 22.2 platform (`spec-platform.md`)

1. A reconciliation row: "Timeline rows and their seq: magpie keeps the events and a rows projection written in the
   same transaction, with a dense row seq and a stable epoch (pigeon 14.1.18); M9/M10/M12 and share reads serve rows;
   pigeon keeps only the pure Row -> PaseoRow mapping." R3's "pigeon spec 14.1's paths" then holds as written.
2. A row "Token-level live frames: not in this release (pigeon 8.7)"; scenario E2 drops "live frames precede the
   durable row" and asserts instead that the reply arrives as whole rows with dense seq and zero catch-ups.
3. 9.3/9.4: pigeon `terminationGracePeriodSeconds: 320` (11.2: transfers finish within 300 s).
4. 5.4: the `aviary-<sha12>` release also carries `pigeon-frames.ndjson` and `paseo-contract.tgz` (18.1, 17.1).
5. 4.5: the source rule that `jarvis`/`yyc` literals appear in pigeon only in `crates/pigeon/src/paseo/compat.rs`.
6. 14: the Cloudflare names pigeon references (17.5) are the ones platform 14.1 already has; cutover 9.1 should use
   them too (tunnel name `pigeon`, output `pigeon_tunnel_token`, path list `staff_ai_paths`).

### 22.3 supervisor (`spec-supervisor.md`)

1. 8.1 and 8.3: no live frames in this release: drop `--include-partial-messages` from the argv (section 6) and the
   `POST /v1/sandbox/turns/{turn}/live` sender; 23 item 1 goes. Should a supervisor still send one (an older image),
   a 404 from magpie means "drop frames for this turn, never retry".
2. 6.4: the MCP server rename to `tools` needs nothing from pigeon (7.8 maps MCP names server-agnostically); the
   supervisor's own tool policy must name the new tools, and history keeps the old names as data.

### 22.4 cutover (`spec-cutover.md`)

1. 6.4 and 6.5: the refusal is 409 `{"code": "conversation_migrating", "message", "retryAfterMs"}` (platform 3.6);
   pigeon's behaviour and sentences are 7.10 and 7.9 here (`legacy.cut` included).
2. 18 items 1-2: decided (14.1 is the shape; magpie owns rows, 14.1.18); 18.1's "this spec uses magpie's shapes"
   becomes "uses pigeon 14.1's shapes", so cutover's magpie calls read `/v1/…` camelCase as in 14.1.
3. 17.2 items 1-7 are applied here (20, 7.10, 5.13, 17.2, 17.5, 18.1, 17.1). Item 5's timing (the path list shrinks
   in P5) and platform 14.2's (in the switch commit) disagree; pigeon's CI test (17.5) passes either way.
4. 9.1: the tofu names of platform 14.1 (22.2 item 6).
5. 13.1: `clients.txt` lives at `testing/paseo-contract/clients.txt`.

### 22.5 hermit (`spec-hermit.md`)

H1-H3 under `/v1` (platform R3), `/ops/*` unprefixed, `HERMIT_ACCEPT_PIGEON`, `Aviary-Person` on `/ops/*` (3.2's
`HERMIT_PIGEON_TOKENS` and `Pigeon-Person` go). Nothing else of hermit's changes for pigeon.

---

## 23. Rejected review points

1. **"magpie serves only events; pigeon projects them into Paseo rows, folding non-row events into the next row's
   `seqStart..seqEnd` or sending seq-carrying no-op rows."** Not taken; magpie owns the rows (14.1.18). The app's live
   gate takes one `seq` per frame and accepts it only when it equals the previous `endSeq + 1`
   (`processTimelineSequencingGate`); it then sets `endSeq = seq`. A row that absorbed event seqs 5-7 must be sent
   with `seq: 7`, which is a gap after 4, or with `seq: 5`, after which the next row (8) is a gap. Paseo has no no-op
   timeline item, and any item sent to fill seqs would render. A dense counter of rows is therefore needed, and a
   stateless pigeon cannot compute it for a page in the middle of a conversation without reading every event before
   it, which breaks "history cost ~ page size". Pairing a tool result that lands in a later page with its call has
   the same problem. The owner of the events can number rows in the event's transaction at no extra cost; that is
   the target design's "pigeon streams by seq" with the seq the app can accept (section 2).
2. **"Build token-level live streaming now (an unlogged live table, a `live` entry, `GET …/live`, a pigeon
   mapping)."** Not taken for this release (8.7). Today staff see whole blocks, so leaving it out is no regression;
   building it needs a correct merge with the durable row in the app's reducer, a mid-message join and supersede rules
   across rollouts, none of which is specified in any spec yet. The additive path is written down in 8.7.
3. **"Make uploads resumable (chunked, upload id plus offset)."** Not taken; the drain lets a moving transfer finish
   (11.2 step 4), which covers a 50 MiB upload on a slow phone link with no app or magpie change.
4. **"Generate the Rust message types with `typify`."** Partly taken: the JSON Schema is generated from zod in CI and
   pigeon's hand-written serde types are checked against it in both directions (18.1); generating the types
   themselves from a 430-message union produces unusable Rust and is not done.

# Platform: repository, build, test harness, YYC deployment, observability (spec)

Status: design for implementation, 2026-11-02. Authoritative inputs: `/home/claude/artifacts/magpie-target-design.md`
(the target design) and the refactor brief's decisions (components, REST only, no deployment switches, Rust, public
DE0CH/aviary, one-time import, zero-downtime deploys). It builds on the component specs written in this run
(`spec-pigeon.md`, `spec-magpie.md`, `spec-hermit.md`, `spec-supervisor.md`) and gap reports 0-3, and on the live
state of YYC's cluster read on 2026-11-02 (read-only: `kubectl get`, `kubectl top`, `pg_stat_*`).

Code references: DE0CH/jarvis3 at 3f7499a (live `jarvis3-chat`), DE0CH/YYC at 1775efc4, DE0CH/claudecodeui at 3de3f5d8.

Review round 2 (2026-11-02): the reconciliation table gained R26-R41 (3.10), the sandbox gateway contract is written
once in 3.11, egress's and tools' changes in 17.2 are now complete (drain, self hosts, upload cap, database outages,
the tools owner header), the ZDR guard and the OpenRouter guardrail script are rewritten (13.6), section 22 records a
disposition for every inventory feature no component spec owns, section 23 says where each review point landed and
section 24 lists the points not taken.

Revision 2026-11-02 (Deyao's decisions, `DECISIONS.md`): `yyc/deploy` is applied by GitHub workflow `yyc-ai.yml`
and Kustomizations `yyc-ai` and `yyc-ai-secrets` go (decision 1; text of record `deploy-workflows.md`, which replaced
13.1, 13.2's reconcile step and 13.4 here); `magpie-pg` is decided (decision 2; manifest pg-ha 4); Cluster `pg` stays
single-instance (decision 10, which revised decision 8: 11.5 is the stated gap again and the HA plan's cost is gone
from 16.4); the licence is AGPL-3.0-or-later (decision 3); the switch-over is DESIGN 9 (decisions 9, 11 and 12, with
the mechanisms of `switch-final.md` it cites; 18 here points to it). Section 21 has no open question
left.

Revision 2026-11-03 (decision 13; text of record DESIGN 15): the new stack deploys onto the new Talos cluster and the
old stack stays on the old k3s cluster, so YYC AI moves between clusters at the switch. Section 25 here states the
platform's side (where each object lives, who applies it, the cross-cluster bridges and their NetworkPolicies,
databases, the two Sealed Secrets certs, sizes); 9.1, 9.11, 10.2, 11, 12, 13.1, 16.2 and 18 are edited for it. Where
any other text here still names the old cluster's nodes, `10.0.0.3:6443`, Flux `yyc-apps`/`yyc-db` for a new-stack
object, `legacy/` inside `yyc/deploy/`, a pod selector across the two stacks, `pg-rw.db` as a new-stack database
host (it is `pg-shared.db`), 10.244.0.0/16 as the new cluster's pod CIDR, an `ipBlock` for an address inside the new
cluster (pods, nodes, the API server: selectors and entities instead, DESIGN 15.2 N3), or fsn1 as required for every
new-stack pod (only egress requires it, DESIGN 15.7), section 25 holds.

This spec owns what is shared by all four services and by the deployment: the repository and its crates, the
conventions every service follows (ports, paths, headers, JSON, credentials, logs, health, drain, versioning), CI and
releases, the integration harness, everything in DE0CH/YYC that deploys YYC AI, observability, sizes, and the split
of DE0CH/jarvis3. Where the component specs disagree with each other, section 3 decides, and the component spec
follows this one.

Contents
1. Decisions in one list
2. Picture
3. Platform conventions (binding on every service), the reconciliation table and the sandbox gateway contract
4. DE0CH/aviary: layout, crates, dependencies
5. Builds, images, releases
6. CI workflows (aviary)
7. Integration harness (all services + Postgres + fake sandboxes in CI)
8. Conformance suites (contracts other repos implement)
9. YYC deployment (DE0CH/YYC `yyc/deploy`)
10. NetworkPolicies
11. Databases (Clusters `magpie-pg` and `pg`)
12. Secrets (Sealed Secrets, `infra/secrets.txt`, store `yyc`)
13. The deploy workflow, the YYC checks and the AMI workflow
14. Cloudflare (tunnel and the switch)
15. Observability: logs, metrics, the ops page and the news-stream monitor
16. Resource sizes and the capacity budget
17. DE0CH/jarvis3: what it keeps, what it loses, what egress and tools must change
18. The switch, platform view (order and rollback)
19. Failure modes of the platform
20. Requirements this spec places on the component specs
21. Open questions (Deyao only)
22. Coverage: inventory features no component spec owns
23. Review round 2: where each point landed
24. Rejected review points

---

## 1. Decisions in one list

1. **One Cargo workspace** in DE0CH/aviary (public, AGPL-3.0-or-later, decision 3, section 4.6): four service crates (`pigeon`,
   `magpie`, `hermit`, `supervisor`), five contract crates (`magpie-api`, `hermit-api`, `supervisor-api`,
   `gateway-api`, `sandboxd-api`; pigeon serves only the app protocol and has none), two runtime crates
   (`aviary-core`, `aviary-pg`), `sandboxd`
   (moved from jarvis3), and the test fakes and conformance tool under `testing/`.
2. **One aviary commit is one release of everything.** CI builds and tests every crate on every push; on `main` it
   pushes every image as `ghcr.io/de0ch/<name>:<sha12>` only after the integration harness passed, and publishes one
   GitHub release `aviary-<sha12>` with the static binaries. YYC pins one aviary commit for pigeon, magpie and hermit
   (a check enforces it) and, separately, the sandbox image by digest in `hermit.json` (it changes only when the
   supervisor or the image changes, so AMIs are not re-baked for every commit).
3. **The sandbox image is built in aviary** (`images/sandbox`, `ghcr.io/de0ch/hermit-sandbox:<sha12>`), with the
   supervisor and keeper compiled from the same commit. jarvis3 loses `chat/sandbox`. Another deployment's sandbox image
   (Jarvis 4, open) takes the same binaries from the `aviary-<sha12>` release.
4. **New namespace `yyc-ai`** in YYC's cluster for the whole new stack (pigeon, magpie, hermit, egress, tools,
   cloudflared). The old stack keeps running untouched in `jarvis` until it drains, then the namespace is deleted
   whole. This is what lets egress change its routes as a clean cut while old sandboxes still use the old egress.
5. **A new Cloudflare tunnel** (tofu resource `pigeon`, tunnel name `yyc-ai-pigeon`) for the new stack; the switch is
   one tofu change of the proxied record `ai.yyc-internal.com` from the old tunnel to the new one (section 14).
6. **Deployments carry no behaviour configuration.** pigeon, magpie and the supervisor get no ConfigMap and no env
   except secrets, bindings (database URL, bucket) and downward-API facts. ConfigMaps exist only for the YYC
   implementations (hermit's `hermit.json`, the catalogue, egress's and tools' own config). A YYC CI check enforces it
   on the rendered manifests (13.3).
7. **Fixed endpoint names are Kubernetes Services on port 80** in the deployment's namespace: `pigeon`, `magpie`,
   `hermit`, `model`, `tools`, `egress`. Every service listens on 8080 (API) and 9090 (health, metrics); 9090 is never
   in a Service.
8. **One credential per caller→callee pair**, accepted as a list so rotation overlaps: callee env
   `<CALLEE>_ACCEPT_<CALLER>`, caller env `<CALLER>_TOKEN_<CALLEE>`, store key `YYC_TOKEN_<CALLER>_<CALLEE>`
   (and `…_NEXT` during a rotation).
9. **Databases**: magpie's in its own CloudNativePG Cluster `magpie-pg` (3 instances, synchronous replication,
   switchover updates; R31, decision 2); `hermit`, `egress`, `tools` on the shared Cluster `pg`, which stays one instance
   (decision 10; its restart is a stated gap, 11.5). Each database has its own role and
   is closed to all others. `jarvis` and `cloudcli` are dropped 30 days after the switch (after the last locked dump that holds
   them).
10. **Integration harness = k3d** (k3s in Docker, the same distribution and NetworkPolicy controller as YYC) running
    the real pigeon, magpie and hermit images, the real sandbox image as Pods created by a fake sandboxd, a reference
    gateway, a mock model, fake tools and a fake Access issuer. It gates every image push.
11. **Readiness never depends on a shared dependency** (Postgres, another service): only on the service's own
    initialisation and on not draining, so a dependency blip never empties a Service.
12. **Observability stays as cheap as today**: JSON logs to stdout read with `kubectl logs` (plus a trace script by
    conversation id), `/metrics` on every service with no scraper installed, the ops page (hermit) as the operator
    view, the news-stream monitor as the alerting path. hermit additionally keeps each released sandbox's last 64 KiB
    of console log (section 15.4), because AWS hosts and their log files die.
13. **jarvis3 keeps egress and tools** (TypeScript, image `jarvis3-chat`), loses `web`, the sandbox image, sandboxd, the
    E2B and Kubernetes providers and its Kustomize base; YYC writes egress's and tools' manifests itself.
15. **Deploys are applied by a GitHub workflow** (decision 1, 13): `yyc-ai.yml` in DE0CH/YYC plans against the live
    state, refuses deletions a commit did not name, applies with server-side apply in a fixed order and waits for every
    rollout; it reaches the API server through tunnel `yyc-deploy` and authenticates with GitHub's OIDC token, so no
    cluster credential is stored in GitHub.
14. **The sandbox gateway contract is written once** (3.11, crate `gateway-api`, conformance 8.2): one addressing mode
    (the proxy), self-verifying sandbox tokens with hermit only for revocation, the routes of the four fixed names,
    the transport limits and the drain rules. hermit, magpie, the supervisor, egress and the harness reference it.

---

## 2. Picture

```
                         Cloudflare edge (Access on /api, /paseo-ws, /ops)
                                        |
                tunnel "yyc-ai-pigeon"  |  (old tunnel "yyc-ai" -> namespace jarvis until teardown)
                                        v
 namespace yyc-ai   cloudflared x2 --> pigeon x2..6 (Service pigeon:80)
                                        |  REST, bearer per pair
                         +--------------+------------------+---------------------+
                         v                                 v                     v
                  magpie x2..6 (magpie:80)  <-------  hermit x2 (hermit:80)   model:80 -> egress:8082
                     |     ^   |                         |  |  \
   db/magpie-pg (3,  |     |   +-- bucket yyc-ai-magpie  |  |   +-- EC2 API, k8s API (read), JuiceFS (PVC)
   synchronous)      |     |                             |  |
   db/pg: hermit,    |     +---- egress x2 (gateway) ----+  +-- sandboxd mTLS :7443 / supervisor :7070
   egress, tools,    |            ^  3128 proxy (the only sandbox port)  (10.202.0.0/24, 10.203.0.0/16)
   JuiceFS metadata  |            |  8082 in-cluster API (model, ops)
                     |            |  VIP 10.202.1.1:3128, DNAT by hermit's fabric to ready egress pods
                     |   sandboxes 10.203.0.0/16 (AWS burst hosts, gVisor) -- WireGuard -- hermit-gw (yyc-ai-fabric DS)
                     |            |  every sandbox client: absolute-form http://magpie|hermit|model|tools/… via the proxy
                     |            v
                     +------ tools x2 (tools:80) --> yyc-mcp.yyc, web.yyc (capture), Exa/TikHub/AnyAPI
```

---

## 3. Platform conventions (binding on every service)

### 3.1 Processes and ports

| Port | Name | Serves | In the Service |
|---|---|---|---|
| 8080 | `http` | the service's API (and, for pigeon, the public site) | yes: Service `<name>` port 80 → 8080 |
| 9090 | `ops` | `GET /healthz`, `GET /readyz`, `GET /metrics`, `GET /version` | never |

Exceptions are listed, not configured: egress (jarvis3) listens on 3128 (the sandbox proxy: the only port a sandbox
reaches, R32), 8082 (in-cluster API: `model` for pigeon, ops for hermit), 9090 (health); the supervisor serves hermit
on 7070 inside the sandbox; sandboxd serves mTLS on 7443 on hosts.

Every binary: `<bin> serve` is the default subcommand; `<bin> --version` prints `"<name> <sha12> protocols=<json>"`;
other subcommands are one-shot jobs (`magpie import …`, `hermit import-jarvis`, `hermit legacy-cleanup`). No flag or
subcommand changes how `serve` behaves.

### 3.2 Health, readiness, version

```
GET :9090/healthz  -> 200 {"ok": true}                     (the event loop answered within 1 s; liveness)
GET :9090/readyz   -> 200 {"ok": true}
                   -> 503 {"ok": false, "why": ["draining" | "migrations" | "<component-specific init>"]}
GET :9090/version  -> 200 {"name": "magpie", "version": "<sha12>", "revision": "<40 hex>",
                           "builtAt": "<RFC 3339>", "provides": {"magpie-sandbox": 2, "magpie-person": 1},
                           "accepts": {"magpie-sandbox": [1, 2]}}
GET :9090/metrics  -> 200 text/plain; version=0.0.4 (Prometheus exposition)
```

Readiness rule (decision 11): `/readyz` is 503 only while the process has not finished its own start (migrations
applied by someone at least once and verified at this build's version, its listener and, for magpie, its first
LISTEN connection established, hermit's config and catalogue loaded once, pigeon's deployment facts and JWKS loaded
once) or while draining. It does **not** turn 503 when Postgres, magpie or hermit are unreachable later; requests then
fail with their own error codes. This overrides magpie spec 16.5 ("Postgres reachable", "LISTEN connected").

### 3.3 Drain (every service, implemented once in `aviary-core`)

1. Kubernetes runs `preStop: sleep <N>` (pigeon, magpie 5 s; hermit 10 s; egress, tools 10 s). The endpoint leaves
   the Service while the process still serves.
2. SIGTERM: `draining = true` (`/readyz` 503), the component's drain hooks run in order (close listeners for new
   connections, end long-polls and streams with their "reconnect" answer, hand back leases and leader locks, wait
   for in-flight work up to the component's bound), then the pool closes and the process exits 0 with log `drained`.
3. `terminationGracePeriodSeconds` = preStop + the component's bound + 15 s (table in 9.3).
4. A second SIGTERM or the grace end: exit at once (the work is designed to be resumable elsewhere).

### 3.4 Paths and versions of the REST APIs

- Every service-to-service REST route is under `/v1/`. A breaking change gets `/v2/` beside `/v1/` (never an
  in-place rename); the old one is deleted once nothing calls it (a clean cut in a later release, not an alias kept
  for its own sake).
- Not under `/v1/`: pigeon's public paths (the app protocol frozen by shipped iOS builds, pigeon spec 1.1), hermit's
  operator pages `/ops/` and `/ops/snapshot` (browser paths forwarded by pigeon), and `:9090/*`.
- **Protocol numbers.** Each contract crate defines `pub const PROTOCOL: u32` per surface (`magpie-person`,
  `magpie-sandbox`, `hermit-core`, `hermit-sandbox`, `supervisor-control`, `gateway`). A server reports what it
  provides and what it accepts (`/version`); clients send `Aviary-Protocol: <surface>=<n>` on every call. A server
  accepts its current and previous number. The supervisor's number is fixed for a sandbox's life (it never upgrades in
  place), so magpie and hermit must keep accepting the previous `*-sandbox` number until every sandbox on the old
  image is gone (at most hermit's 8 h + 1 h grace, or `POST /v1/admin/recycle`).
- Images carry the same facts as labels (5.3), so YYC's CI can refuse a pin set that does not fit together (13.3).

### 3.5 Headers

One prefix, `Aviary-`, for every header the services define (replacing `X-Magpie-*`, `Pigeon-*`, `Sandbox-*`,
`X-Sandbox-*`, `Magpie-Request`, `Admin-Actor` in the component specs):

| Header | Set by | Meaning |
|---|---|---|
| `Authorization: Bearer <pair token>` | every caller | which service is calling (3.7) |
| `Aviary-Request: <uuid v7>` | pigeon at the edge, the gateway for sandbox calls, hermit for its own work | correlation id, forwarded unchanged on every onward call and logged as `request` |
| `Aviary-Protocol: <surface>=<n>` | every client | 3.4 |
| `Aviary-Person: <email>` | pigeon | the signed-in person (magpie person routes, hermit `/ops/*`) |
| `Aviary-Viewer: <admin email>`, `Aviary-View-Session: <≤64 chars>` | pigeon | an admin viewing as the person |
| `Aviary-Admin: <email>` | the admin tooling | the operator behind an admin route (recorded) |
| `Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation`, `Aviary-Sandbox-Person` | the gateway only | the verified sandbox behind a call on a fixed name (`/v1/sandbox/**` to magpie and hermit, `/mcp` to tools, the model route); `-Person` is the owner's trimmed, lowercased email (3.11.3) |
| `Aviary-Lease: <uuid>` | the supervisor | magpie's single-writer lease (magpie 6.9) |
| `Aviary-Upload-Name` (percent-encoded), `Aviary-Upload-Kind: file\|image` | pigeon | upload metadata (magpie M13) |

Rules: every receiving service strips client-supplied `Aviary-*` headers that its caller role may not set (a person
request reaching pigeon, a sandbox request reaching the gateway) before trusting or forwarding; a callee trusts a
header only together with the bearer of the caller allowed to set it.

### 3.6 JSON and errors

- `content-type: application/json; charset=utf-8`; field names **camelCase** in every request and response of every
  service (serde `rename_all = "camelCase"` on every type in the `*-api` crates; a unit test serialises every fixture
  and fails on a key containing `_`). Query parameters camelCase too (`waitMs`, `afterVersion`). Database columns stay
  snake_case inside each service. This overrides magpie spec 16's snake_case bodies; the ops snapshot's camelCase
  fields (the monitor contract) and Paseo's are already camelCase.
- Unknown request fields are ignored; clients ignore unknown response fields; fields are added, never renamed.
- Times: RFC 3339 UTC with milliseconds (`2026-11-02T04:05:06.789Z`). Ids: lowercase UUIDs (v7 where a service mints
  ordered ids). Emails: trimmed and lowercased before any comparison.
- Error envelope, every non-2xx JSON answer: `{"code": "<lower_snake_case>", "message": "<English, for logs>",
  …type-specific fields}`. HTTP codes: 400 `bad_request`, 401 `unauthenticated` (+ `www-authenticate: Bearer`),
  403 `wrong_caller` (valid token, route not allowed for that caller) or a domain refusal, 404 `not_found`, 409
  domain conflicts, 410 `cursor_expired`, 413 `too_large`, 415 `unsupported_type`, 429 `rate_limited` (+
  `retry-after`), 503 `unavailable` / domain busy codes (+ `retry-after`), 507 `quota_exceeded`. Domain codes stay as
  each spec names them, lowercased (magpie's `UNAUTHORIZED`, `TOO_LARGE`, `QUOTA_EXCEEDED`… become `unauthenticated`,
  `too_large`, `quota_exceeded`). Turn and event error codes inside events (`sandbox.lost`, `run.oom`) are data, not
  HTTP codes, and keep their dotted form.
- The envelope is flat: never `{"error": {…}}`. Example, the switch window's re-route answer (R37):
  `409 {"code": "conversation_migrating", "message": "conversation is owned by the old stack", "route": "legacy"}`.
- Every response: `cache-control: no-store` unless the route says otherwise; `x-content-type-options: nosniff`.

### 3.7 Caller credentials

- A pair token is 32 random bytes, base64url without padding (43 characters).
- The callee reads `<CALLEE>_ACCEPT_<CALLER>`: a comma-separated list (current first; two during a rotation).
  Comparison: SHA-256 of the presented token against SHA-256 of each listed value, constant-time
  (`ring::constant_time::verify_slices_are_equal`). A missing env for a caller the service knows is a start failure
  (logged by name, never by value); an empty value is a start failure too.
- The caller reads `<CALLER>_TOKEN_<CALLEE>`: one value.
- `aviary-core::auth` implements both and the route-to-caller table each service declares in code (no
  configuration).
- Rotation (zero downtime, any pair): (1) put `YYC_TOKEN_<CALLER>_<CALLEE>_NEXT` in the store; reseal the callee's
  Secret with both values (`…=A|B`, 12.1), bump the callee's `secrets-rev`, roll; (2) reseal the caller's Secret with
  the next value, roll the caller; (3) move `_NEXT` into the main key, drop `_NEXT`, reseal the callee with one value,
  roll. Each roll is a normal zero-downtime rollout.

The pairs (YYC):

| Caller → callee | Callee env | Caller env | Store key |
|---|---|---|---|
| pigeon → magpie | `MAGPIE_ACCEPT_PIGEON` | `PIGEON_TOKEN_MAGPIE` | `YYC_TOKEN_PIGEON_MAGPIE` |
| pigeon → hermit | `HERMIT_ACCEPT_PIGEON` | `PIGEON_TOKEN_HERMIT` | `YYC_TOKEN_PIGEON_HERMIT` |
| pigeon → model (egress) | `EGRESS_ACCEPT_PIGEON` | `PIGEON_TOKEN_MODEL` | `YYC_TOKEN_PIGEON_MODEL` |
| magpie → hermit | `HERMIT_ACCEPT_MAGPIE` | `MAGPIE_TOKEN_HERMIT` | `YYC_TOKEN_MAGPIE_HERMIT` |
| magpie → magpie-import (the synchronous ingest `POST /v1/ingest-upload` of magpie 8.2 step 4, A3 to A6; WP-51) | `MAGPIE_IMPORT_ACCEPT_MAGPIE` (the `export-server` sidecar) | `MAGPIE_TOKEN_IMPORT` (required at magpie's start from P1 until the A6 release removes it) | `YYC_TOKEN_MAGPIE_IMPORT` |
| hermit → magpie (ops reads, exists) | `MAGPIE_ACCEPT_HERMIT` | `HERMIT_TOKEN_MAGPIE` | `YYC_TOKEN_HERMIT_MAGPIE` |
| hermit → egress (ops summary) | `EGRESS_ACCEPT_HERMIT` | `HERMIT_TOKEN_EGRESS` | `YYC_TOKEN_HERMIT_EGRESS` |
| gateway (egress) → magpie | `MAGPIE_ACCEPT_GATEWAY` | `EGRESS_TOKEN_MAGPIE` | `YYC_TOKEN_GATEWAY_MAGPIE` |
| gateway (egress) → hermit | `HERMIT_ACCEPT_GATEWAY` | `EGRESS_TOKEN_HERMIT` | `YYC_TOKEN_GATEWAY_HERMIT` |
| gateway (egress) → tools | `TOOLS_ACCEPT_GATEWAY` | `EGRESS_TOKEN_TOOLS` | `YYC_TOKEN_GATEWAY_TOOLS` |
| tools → hermit (sandbox liveness and state, 17.2 tools item 2) | `HERMIT_ACCEPT_TOOLS` | `TOOLS_TOKEN_HERMIT` | `YYC_TOKEN_TOOLS_HERMIT` |
| monitor (news stream) → hermit | `HERMIT_ACCEPT_MONITOR` | `HERMIT_OPS_TOKEN` (monitor.py) | `YYC_TOKEN_MONITOR_HERMIT` |
| admin (Claude, runbooks) → hermit | `HERMIT_ACCEPT_ADMIN` | (store only) | `YYC_TOKEN_ADMIN_HERMIT` |
| admin → magpie | `MAGPIE_ACCEPT_ADMIN` | (store only) | `YYC_TOKEN_ADMIN_MAGPIE` |
| admin → egress | `EGRESS_ACCEPT_ADMIN` | (store only) | `YYC_TOKEN_ADMIN_EGRESS` |

hermit → supervisor uses the per-sandbox control token (hermit spec 6.2); supervisor → magpie/hermit/model/tools uses
the per-sandbox proxy credential through the gateway (hermit spec 6.1). Neither is a stored secret.

Secret names: each component's Kubernetes Secret is named after the component (`pigeon`, `magpie`, `hermit`, `egress`,
`tools`, `tunnel`), and a one-off job's after the job (`magpie-import`, `hermit-import`, `egress-import`,
`tools-import`); no `-env` suffix. Env names of bindings are `<SERVICE>_<THING>` (`MAGPIE_DATABASE_URL`), of an
importer's sources `<SERVICE>_IMPORT_<SOURCE>_URL` (`MAGPIE_IMPORT_CLOUDCLI_URL`).

### 3.8 Identity in logs

`person_key` = `"u"` + the first 24 hex digits of SHA-256 of the normalised email (today's `userKey`, so log
searches carry over). Logs never carry emails, message text, file names, tokens, cookies or image data. hermit's
12-hex `person_key` (hermit spec 25) becomes this one.

### 3.9 TLS and outbound HTTP

- Inbound: plain HTTP inside the cluster (NetworkPolicies decide who connects); TLS ends at Cloudflare and at
  cloudflared. sandboxd's mTLS stays (it crosses the internet over WireGuard).
- Outbound: `reqwest` with rustls and the system trust store (`rustls-native-certs`, which honours `SSL_CERT_FILE`;
  the harness uses that to trust its fake Access issuer, 7.4). HTTP/1.1 keep-alive pools; every call has its own
  timeout set where the component spec names it; retries only on idempotent calls or with the caller's idempotency
  key.
- Postgres: TLS required (`sslmode=require`), **no CA pinning**: CloudNativePG's operator-issued CA is short-lived and
  renewed by the operator, so a CA copied into a Secret would expire; the path stays on the cluster's private network
  behind `pg-ingress`. This drops hermit spec's `HERMIT_DB_CA`.

### 3.10 Reconciliation table (where component specs disagreed)

| # | Topic | Specs said | Platform decision |
|---|---|---|---|
| R1 | Crate paths | hermit: `aviary/hermit-contract`, `aviary/hermit`, `aviary/sandboxd`, `aviary/sandbox`; magpie/supervisor: `crates/…`; pigeon: `aviary/pigeon` | all crates under `crates/`; contract crates named `<service>-api` (`hermit-contract` → `hermit-api`); host scripts under `fabric/`; images under `images/` (section 4) |
| R2 | Ports | magpie health 8081, Service `magpie:8080`; pigeon/hermit 9090 and Service port 80 | 8080 API, 9090 ops, Service port 80, everywhere (3.1) |
| R3 | Route prefix | magpie `/v1/…`; hermit unprefixed (`/deployment`, `/self`, `/admin/*`); pigeon's consumed magpie shapes unprefixed | every REST route under `/v1/` (3.4); hermit's become `/v1/deployment`, `/v1/people/{email}`, `/v1/warm`, `/v1/conversations/{id}/sandbox…`, `/v1/sandbox-events`, `/v1/workspaces/…`, `/v1/sandbox-token-keys`, `/v1/sandbox-tokens/{sandboxId}` (R33), `/v1/admin/…`; magpie's pigeon routes are pigeon spec 14.1's paths under `/v1/` |
| R4 | The sandbox's hermit routes | hermit `/self`, `/self/secrets/{key}`, `/self/activity`; supervisor `/v1/sandbox/self`, `/v1/sandbox/secrets/{key}`, `/v1/sandbox/activity` | the supervisor's names. The gateway forwards only `/v1/sandbox/**` to magpie and to hermit |
| R5 | Identity headers | `X-Magpie-Person`, `Pigeon-Person`, `X-Sandbox-Id`, `Sandbox-Id`, `Admin-Actor`, `X-Magpie-Lease`… | `Aviary-*` (3.5) |
| R6 | JSON case, error codes | magpie snake_case bodies and UPPER codes; pigeon/hermit camelCase and lower codes | camelCase bodies, lower_snake_case HTTP codes (3.6) |
| R7 | Caller credential env | magpie `MAGPIE_CALLERS` JSON of hashes; hermit `HERMIT_PIGEON_TOKENS`… | `<CALLEE>_ACCEPT_<CALLER>` lists of tokens, `<CALLER>_TOKEN_<CALLEE>` (3.7) |
| R8 | Sandbox events hermit → magpie | magpie 8.7: hermit POSTs `/v1/hermit/sandbox-events`; hermit 3.6.5: magpie reads hermit's feed `GET /sandbox-events` | the pull feed: magpie's job leader follows `GET http://hermit/v1/sandbox-events?after=<seq>&waitMs=25000`, storing its cursor in its own database; magpie's push route is not built; magpie keeps `POST /v1/hermit/persons/{person}/purge` |
| R9 | Postgres client | magpie sqlx; hermit tokio-postgres + deadpool | `tokio-postgres` + `deadpool-postgres` through `aviary-pg` for all (no compile-time query checking, so no database at build time and no offline query cache to keep in step) |
| R10 | DB TLS | hermit pins CNPG's CA | `sslmode=require`, no CA pin (3.9) |
| R11 | Pool sizes | magpie 20 + 2; hermit 12 | magpie 12 + LISTEN + leader; hermit 10 + leader + listener (budget in 16.3) |
| R12 | Sandbox entrypoint | hermit 17: `tini -- supervisor-keeper`; supervisor 3: keeper is PID 1 | keeper is PID 1, no tini (it reaps and owns stdin pipes by design) |
| R13 | Egress VIP | supervisor 17.1 example 10.201.1.1 (old fabric) | 10.202.1.1 (hermit 13.1); the supervisor reads the proxy, with its credential, from its boot environment (`HTTP_PROXY`, hermit 5.1) and hard-codes nothing |
| R14 | Namespace | all specs: `jarvis` | `yyc-ai` (decision 4); in-cluster DNS `*.yyc-ai.svc.cluster.local` |
| R15 | Importers | magpie: separate `magpie-import` crate and Job; hermit: `hermit import-jarvis` subcommand; cutover: own image `magpie-import`, own workflow, a long-running Deployment | subcommands of their service's image, run the way cutover 7-8 and switch-final need: Jobs in A2; Deployment `magpie-import` (2 replicas, leader-locked, command `magpie import handover`, image `magpie`, with the writer sidecar `export-server`) from A3 until P4 ends; its one-shot commands (cutover 7.9's list) by `kubectl exec deploy/magpie-import -- magpie import <cmd>`; CronJobs `hermit-import-jarvis`, `egress-import-jarvis`, `tools-import-jarvis` for the deltas (9.12). `crates/magpie-import` is a library linked into `magpie`; no `magpie-import` image and no `magpie-import.yml` (R17). Deleted with the subcommands after the switch |
| R16 | Releases | hermit: `sandboxd-<sha12>`; supervisor: `supervisor-<sha12>` | one release `aviary-<sha12>` with every static binary and `SHA256SUMS` (5.4); host scripts fetch `sandboxd` from it |
| R17 | Workflows | one per component (`pigeon.yml`, `magpie.yml`, `hermit.yml`, `sandbox.yml`, `sandboxd.yml`, `supervisor.yml`) | one `ci.yml` for every push plus `nightly.yml` (section 6): shared crates make per-path triggers wrong |
| R18 | Model facts | pigeon: `GET http://model/v1/models`; supervisor: `GET http://model/v1/aviary/models` | one route `GET http://model/v1/aviary/models` → `{"default", "small", "title", "contextTokens", "models": [{"id", "label", "reasoning"}]}` for both (pigeon in-cluster with its pair token, the supervisor through the gateway) |
| R19 | magpie ops reads | magpie: `/v1/admin/people`, `/v1/admin/view-as-log`; hermit consumes `/ops/people`, `/ops/view-as` | magpie serves `GET /v1/ops/people?tz=`, `GET /v1/ops/view-as?limit=`, `GET /v1/ops/stats` to callers `hermit` and `admin` in the shapes of 15.3; content-bearing reads stay `/v1/admin/conversations/**` for `admin` only |
| R20 | egress ops | hermit: `GET http://egress/ops/summary` | `GET http://egress/v1/ops/summary?tz=&recentMinutes=` (Service `egress` port 80 → 8082) |
| R21 | Pull secrets | `ghcr-pull` on every pod | every aviary and jarvis3 image public; no pull Secret in `yyc-ai`; the AWS hosts' `crane` pulls anonymously |
| R22 | Readiness | magpie: Postgres reachable | 3.2 |
| R23 | Request id header | `Pigeon-Request`, `Magpie-Request`, `Request-Id` | `Aviary-Request` |
| R24 | Privileged DaemonSets | hermit: `hermit-gateway` and the overlay route in `kube-system` | namespace `yyc-ai-fabric` (PSS privileged, nothing else in it), so the fenced Sealed Secrets applier never needs `kube-system` (9.6) |
| R25 | Monitor auth | hermit: `HERMIT_OPS_TOKENS` | `HERMIT_ACCEPT_MONITOR`; monitor.py reads `HERMIT_OPS_TOKEN` (15.5) |
| R26 | cc write retry | supervisor EIO after 10 min; magpie retry for ever | unbounded under general unavailability; one op stuck 10 min while the heartbeat succeeds → EIO and `cc.write_stuck`, resync from the write-through cache (magpie 6.12, supervisor 9.5) |
| R27 | Timeline rows | magpie served events and left projection to pigeon; pigeon 14.1.18 wanted rows from magpie; cutover 18.2 leaned to pigeon | **magpie owns the rows**: a `rows` projection written in the event's transaction under the conversation lock, a dense `rowSeq` per conversation (a tool row re-issued at the next `rowSeq` when its result, status or sent files land), a stable `epoch` changed only when a retract removes or rebuilds a visible row; M9 (`/v1/conversations/{id}/timeline`, `waitMs` long-poll, no NDJSON), M10 and the share read (M16) serve rows; the change feed `/v1/changes` carries `turn{phase, timelineSeq}`, `sandbox`, `ask`, `ask_resolved`, `timeline_replaced` in the same transaction (magpie 7.3-7.6). pigeon keeps only the pure Row → PaseoRow mapping (pigeon 7.8). The events stay magpie's record (admin, import verification) |
| R28 | Workspace snapshots in YYC | supervisor: shares "behind the snapshot calls"; hermit: snapshot 501 | YYC's hermit `snapshot` stays 501; share freeze and continue use only `POST /v1/workspaces/{wid}/clone` |
| R29 | `link(2)` under `~/.claude` | EPERM vs ENOTSUP | EPERM in both specs |
| R30 | Turn origin | `user`, `cli`, `person`, `agent` | `person` \| `agent` \| `import` everywhere |
| R31 | magpie's database | platform: Cluster `pg`; magpie 18.2: own cluster | own Cluster `magpie-pg` in `db`: 3 instances on `pool=apps` nodes, `postgresql.synchronous: {method: any, number: 1, dataDurability: required, failoverQuorum: true}`, `primaryUpdateMethod: switchover`, 10 Gi each, the same image as `pg` (magpie 18.2 plus pg-ha 4), decided (decision 2, EUR 2.0592 per month). An offset mismatch after lost rows is a repair (supervisor re-sends the cached suffix), never a stop (magpie 6.12) |
| R32 | How a sandbox reaches the fixed names | hermit 6.5: the proxy only; supervisor 17.1: direct `http://<name>` via `/etc/hosts` to the VIP and egress :8081; platform: both | **one addressing mode, the proxy** (3.11.1): every sandbox client sends absolute-form `http://<name>/…` to the proxy in its boot environment with `Proxy-Authorization: Basic sbx:<token>`; no :8081 listener, no `Host`-routed fixed names, no `/etc/hosts` entries for the names, no `HERMIT_SANDBOX_TOKEN` (the token travels only as the proxy credential, hermit 5.1) |
| R33 | Sandbox token verification | platform: `GET hermit/v1/sandbox-tokens/{sha256}` cached ≤ 1 s; hermit 6.1-6.3: self-verifying tokens | hermit's: Ed25519-signed `hst1.` tokens checked locally with hermit's published keys (`GET /v1/sandbox-token-keys`); hermit consulted only for liveness (`GET /v1/sandbox-tokens/{sandboxId}`), cached 30 s and served stale without limit while hermit fails; a hermit problem never yields 401/403/407/5xx for a valid token (3.11.2) |
| R34 | Release reasons | free strings in each spec | hermit's `ReleaseReason` enum plus `code` and a free `note`, shared by hermit's feed, the :7070 release call and the supervisor (hermit 13.13) |
| R35 | supervisor ↔ hermit types | both specs claimed the shapes | `hermit-api` owns `SandboxSelf`, `ActivityReport`, `ActivityReply` (text of record: hermit 3.7); `supervisor-api` owns the :7070 surface (text of record: supervisor 15.4; `GET /v1/health` with bearer → `{ok, ready, protocol, version, bootId, busy, state}`, `POST /v1/lifecycle/release {reason, code, force, deadlineMs}` → 200 `{released: true}` \| 409 `sandbox_busy`). Each spec quotes the other's; the crates' golden fixtures settle any wording difference |
| R36 | Token-level live frames | supervisor sends them; magpie 7.5 receives them; pigeon 8.7 does not render them in this release | magpie builds the receiving side (7.5: `POST /v1/sandbox/turns/{turnId}/live`, `live` in M9, `GET …/live`); **in this release the supervisor does not pass `--include-partial-messages` and sends no frames, and pigeon renders none**; a 404 or 409 `turn_not_active` on `/live` means "drop frames for this turn", never retried. Streaming ships later as one change of supervisor and pigeon behind a new `magpie-person` protocol number, after pigeon's real-reducer contract test (pigeon 8.7). E2 asserts whole rows (7.8) |
| R37 | Migration refusal (the switch window) | cutover 6.4: `{"error":{"code":"CONVERSATION_MIGRATING","retry_after_ms"}}` | `409 {"code": "conversation_migrating", "message", "route": "legacy"}` (3.6), sent only for a native mutation of an unfenced legacy conversation, which pigeon re-routes through its legacy upstream at once; a fenced conversation's mutation waits ≤ 3 s instead; nothing reaches a tab, and there is no retry loop, no "being moved" sentence and no `legacy.cut` (switch-final, DESIGN 12 D14); cutover's magpie calls use the `/v1` camelCase shapes of pigeon 14.1 |
| R38 | Gateway drain | egress 120 s then sockets destroyed (A16.F8) | 3.11.6: refuse new connections, `Connection: close` on kept-alive ones, model streams and CONNECT tunnels finish up to 600 s, fixed-name requests up to 60 s, tools route timeout ≥ 180 s; egress grace 625 s |
| R39 | Cloudflare names | cutover: tunnel `yyc-ai-pigeon`, output `tunnel_token_pigeon`, `pigeon_paths`; platform: tunnel `pigeon`, `pigeon_tunnel_token`; pigeon: `pigeon.jarvis.svc` | resource `cloudflare_zero_trust_tunnel_cloudflared.pigeon`, tunnel name `yyc-ai-pigeon`, output `tunnel_token_pigeon` (the existing `tunnel_token_<name>` convention of `outputs.tf`), store key `YYC_CF_PIGEON_TUNNEL_TOKEN`, one path list `local.staff_ai_paths` for both tunnels and the Access app, ingress `http://pigeon.yyc-ai.svc.cluster.local:80` (14) |
| R40 | The catalogue ConfigMap `yyc/yyc-catalogue` | hermit 21.5: key renamed to `catalogue.json`; cutover: moved in P5.4 | the key stays `config.json`, sourced from the new file (`files: [config.json=catalogue.json]`); every reader of the file moves in the switch commit (cutover P3 = 18.2 here), as hermit 21.5's table lists; P5.4 only deletes `config.json` (9.2) |
| R41 | egress and tools manifests | cutover P1: the jarvis3 remote base made namespace-free; platform: YYC writes them | YYC writes them (9.7), pinning only the `jarvis3-chat` image; jarvis3's `deploy/chat` is deleted (17.1); the ZDR guard reads the jarvis3 commit from that image tag (13.6) |

### 3.11 The sandbox gateway contract (crate `gateway-api`; checked by `aviary-conformance gateway`, 8.2)

This is the one statement of the contract. hermit 6.3 (hermit's half: tokens and liveness), magpie 3.3 and 21.2,
supervisor 17 and 23, and egress (17.2) refer here. Owner: the platform (this section, the `gateway-api` crate and the
conformance suite). Implementations: `testing/test-gateway` (the reference, 7.3); in YYC two parts, **egress** (HTTP:
everything below) and **hermit's fabric** (network: the WireGuard hub `hermit-gw` in `yyc-ai-fabric` routes the
sandbox overlay and DNATs the VIP `10.202.1.1:3128` to the ready egress pods' 3128, hermit 6.5, 13.8).

#### 3.11.1 Addressing (R32)

- One mode: an HTTP proxy. The sandbox's boot environment (hermit 5.1) sets `http_proxy`, `https_proxy`,
  `HTTP_PROXY`, `HTTPS_PROXY` = `http://sbx:<sandbox token>@<proxy address>` (YYC `10.202.1.1:3128`) and
  `NO_PROXY=no_proxy=localhost,127.0.0.1,::1`. Every client (the supervisor, Claude Code, pip, npm, curl) sends
  absolute-form requests (`GET http://magpie/v1/sandbox/…`) or `CONNECT host:443` to it.
- The fixed names are `magpie`, `hermit`, `model`, `tools`. They are recognised by exact host match (no port, or port
  80) and never resolved by DNS (the sandbox has none).
- The proxy's own addresses (`selfHosts`: the VIP and its Service names) serve the implementation's local paths (YYC:
  the package mirror) for proxy-form requests whose host is one of them.
- Order of decision for each request: (1) credential, (2) fixed name → its route, (2b) configured route (3.11.8) → its
  upstream, (3) self host → local paths, (4) everything else → the implementation's internet rules (YYC: port, allowlist, threat DNS, public address, upload
  cap). A fixed name or a self host never reaches the internet checks, so the AddressGuard never sees the VIP.

#### 3.11.2 Credential and verification (R33)

1. `Proxy-Authorization: Basic base64("sbx:" + token)` (also accepted as `Authorization` on self-host paths, which pip
   and npm send from the URL's userinfo). Missing → 407 with `Proxy-Authenticate: Basic realm="aviary-gateway"`.
2. Local verification, no network call: the `hst1.<kid>.<payload>.<sig>` token parses, its `kid` is in the cached key
   set (`GET http://hermit/v1/sandbox-token-keys`, refreshed every 5 min and at most every 10 s on an unknown kid, last
   set kept indefinitely on a failed fetch), the Ed25519 signature verifies, the expiry has not passed, and the
   token's source IPv4 equals the connection's source address (skipped when the token carries 0.0.0.0). A failure is
   definitive: 403 `{"code": "not_allowed"}`.
3. Liveness: `GET http://hermit/v1/sandbox-tokens/{sandboxId}` (bearer of the gateway→hermit pair, timeout 1 s),
   cached 30 s and refreshed in the background; `live: false` or 404 → 403 (cached 30 s). hermit unreachable, slow or
   5xx: the last answer is served whatever its age; with no answer yet, the request is admitted on step 2 and the
   lookup retried in the background. **A hermit problem never turns a valid token into 401, 403, 407 or 5xx.**
   Revocation takes effect within 30 s.

#### 3.11.3 Routes

| Name | Paths | Upstream | Added | Body limit |
|---|---|---|---|---|
| `magpie` | `/v1/sandbox/**` only (else 403 `not_allowed`) | `http://magpie` | gateway→magpie pair bearer | 64 MiB per request |
| `hermit` | `/v1/sandbox/**` only | `http://hermit` | gateway→hermit pair bearer | 64 MiB per request |
| `model` | `POST /v1/messages`, `POST /v1/messages/count_tokens`, `GET /v1/aviary/models` (R18) | the implementation's model (YYC: OpenRouter with the injected key, forced ZDR, the shortlist, the budget) | the model credential | streamed, never held (YYC: rewritten as it streams, `ModelBodyRewriter`; 1 GiB bound) |
| `tools` | `GET /v1/tools`, `POST /v1/tools/<name>`, `GET /v1/openapi.json` (decision 15) | `http://tools/v1/tools/…` | gateway→tools pair bearer | tools' own 2 MB cap |

On every fixed-name request the gateway strips the client's `Authorization`, `Proxy-Authorization`, `x-api-key` and
every `Aviary-*` header, then adds `Aviary-Sandbox-Id`, `Aviary-Sandbox-Conversation`, `Aviary-Sandbox-Person` (the
owner's trimmed, lowercased email, from the verified token), `Aviary-Request` (kept if the client sent a valid v7
id, else new) and the callee's bearer. magpie and hermit check the three headers against their own rows.

#### 3.11.4 Transport

- Request and response bodies streamed both ways without buffering (SSE and NDJSON included); first byte relayed
  within 200 ms of the upstream's.
- Responses held open up to 30 s for long-polls (magpie's command poll and M9 wait 25 s); idle timeout 120 s; the
  `tools` route's request timeout ≥ 180 s (above tools' 120 s upstream cap and 150 s drain); the `model` route has no
  total timeout (the client's own, Claude Code's 600 s, bounds it).
- HTTP/1.1 keep-alive both ways; pools to magpie and hermit kept warm (these names carry every FUSE write, report and
  command poll).
- The per-connection upload cap (YYC `maxUploadBytes`, 8 MiB, A6.F8) applies **only** to internet traffic: a CONNECT
  tunnel's or a forward request's client-to-upstream bytes. Fixed names and self-host paths are never counted against
  it; their limit is per request (the table above), so a supervisor keep-alive connection carrying many transcript
  writes and sent-file uploads is never cut with `deny:upload`.
- Hot-path cost (fixed names `magpie`, `hermit`): token checked from memory, no per-request log row (an aggregate
  counter per sandbox and route in `/metrics` instead), added latency p99 ≤ 5 ms. `model`, `tools` and internet
  requests keep one log row each (YYC: `egress_log`).

#### 3.11.5 Errors

The gateway's own refusals are JSON `{"code", "message"}` (3.6) on fixed names and plain text with today's wording on
internet requests (Claude Code shows them to the model, A6.F4). An upstream's answer is relayed unchanged. An upstream
that cannot be reached gives 502 `{"code": "upstream_unreachable"}`; a draining gateway replica's refusal of a new
request is 503 `{"code": "unavailable"}` with `retry-after: 1`.

#### 3.11.6 Drain (R38; fixes A16.F8, A16.F9)

On SIGTERM, after the preStop sleep (the replica has already left the VIP's DNAT targets, which follow the
EndpointSlice's ready endpoints, hermit 13.8):
1. `/readyz` 503; stop accepting new TCP connections.
2. A new request arriving on a kept-alive connection is served and answered with `Connection: close` (the client's
   next request opens a connection that lands on another replica); an idle kept-alive connection is closed at once.
3. In-flight work finishes: `model` streams and CONNECT tunnels up to **600 s** (Claude Code's own request timeout is
   600 s, so no model stream it is waiting on outlives that); fixed-name requests to magpie and hermit (25 s
   long-polls, cc writes, uploads) up to 60 s; `tools` calls up to 180 s.
4. Whatever is still open at the bound is closed and counted in `<gw>_drain_cut_total{kind}` (`/metrics`), then the
   process exits 0 with log `drained`.
`terminationGracePeriodSeconds` = preStop 10 + 600 + 15 = 625 s. A rollout of two replicas therefore takes at most
about 21 minutes and cuts nothing that finishes within 10 minutes. A cut model stream is retried by Claude Code on a
new connection (`api_retry`); a cut fixed-name call is retried by the supervisor with the same `opId` (supervisor 19).

#### 3.11.7 In-cluster API (not for sandboxes)

`GET /v1/aviary/models` for pigeon's bearer (Service `model`), `GET /v1/ops/summary` for hermit's (15.3) and admin
reads (17.2 item 4), on the implementation's in-cluster port (YYC: egress 8082, Services `model` and `egress` port 80).


#### 3.11.8 Configured routes (WP-61; decision 15)

WP-61's choice, recorded here: **a generic configured route kind**, not a fifth fixed name, so the platform names no
deployment's service and other deployments configure none. A deployment may add names beside the four fixed ones,
each to a service of its own that the sandbox calls directly with `curl` as its managed skills say (YYC: `labels`, the
label store's REST API at `https://labels.yyc-internal.com/api/v1/`). `gateway-api::routes` is the shared statement:

- A name is one lowercase DNS label (`[a-z][a-z0-9-]{0,62}`, not ending in `-`) that is not a fixed name; recognised
  exactly as a fixed name is (exact host, ASCII case-insensitive, no port or port 80, never DNS, never `CONNECT`),
  after the fixed names and before the self hosts.
- Only its path prefixes are routed: each prefix is `/…/`, the request path must be clean (3.11.3's rule: no dot or
  empty segment, no percent-encoded `.`, `/` or `\`) and start with one of them; any method. Anything else is 403
  `not_allowed`, not forwarded.
- Headers as on a fixed name (3.11.3): the client's `Authorization`, `Proxy-Authorization`, `x-api-key` and
  `Aviary-*` stripped, the identity triple and `Aviary-Request` added; then the route's own credential, which the
  implementation's configuration names (YYC egress.json: `inject`, the Access service token `yyc-labels-ai`), and
  any implementation-specific identity headers (YYC egress.json `identityHeaders`: `X-Jarvis-Conversation` ← the
  verified conversation, which the label store keys on).
- Limits: 64 MiB per request body; the `tools` route's timeout (≥ 180 s) and drain bound (180 s); one log row per call
  (YYC `egress_log` kind `route`).
- Checked by `aviary-conformance gateway --route <name>` (the gateway routes the name to the suite's fakes with the
  prefix `/api/v1/` and `Authorization: Bearer <gatewayToRoute>`): `route.configured` (GET and POST reach the fake with
  that bearer alone, the triple, path, query and body unchanged) and `route.configured-paths` (outside or unclean
  paths 403, not forwarded). `test-gateway --route <name>=<upstream>,<prefix>…` is the reference.

---

## 4. DE0CH/aviary: layout, crates, dependencies

### 4.1 Tree

```
aviary/
  README.md                 what the four services are, the endpoint names, how to run the harness
  LICENSE                   AGPL-3.0-or-later (4.6)
  Cargo.toml                workspace (4.2)
  Cargo.lock                committed
  rust-toolchain.toml       channel = "<x.y.z>" (the stable release current at creation, recorded exactly),
                            components rustfmt, clippy; targets x86_64-unknown-linux-musl
  .cargo/config.toml        musl target uses the self-contained rust-lld linker; build.rustflags --remap-path-prefix
  deny.toml                 cargo-deny: licenses (AGPL-compatible allowlist), advisories (deny), bans (one version
                            of tokio, hyper, rustls), sources (crates.io only)
  rustfmt.toml              max_width = 120
  crates/
    aviary-core/            the service runtime (4.3)
    aviary-pg/              Postgres pool, migrations, leader lock, LISTEN helper (4.3)
    magpie-api/             magpie's types and client (all three surfaces: person, sandbox, hermit/ops/admin)
    hermit-api/             hermit's contract types, client, protocol numbers (was hermit-contract)
    supervisor-api/         the supervisor's :7070 types and client, the local socket protocol types
    gateway-api/            the gateway contract: proxy credential format, Aviary-Sandbox-* headers, the fixed
                            names, the models route types (R18), the token lookup types
    sandboxd-api/           sandboxd's REST types (shared by sandboxd, hermit's client and fake-sandboxd)
    pigeon/                 bin pigeon (pigeon spec)
    magpie/                 lib + bin magpie (magpie spec), subcommand `import`
    magpie-import/          lib: the one-off importer (magpie spec 19); removed after the switch
    hermit/                 bin hermit (hermit spec: the YYC implementation)
    supervisor/             bins supervisor, supervisor-keeper (supervisor spec)
    sandboxd/               bin sandboxd (moved from jarvis3 chat/sandboxd/daemon with its history notes)
  fabric/
    host/                   setup.sh, gateway.sh, gateway-net.sh, make-certs.sh, fetch-sandboxd.sh (from jarvis3
                            chat/sandboxd/host, renamed per hermit spec 14)
    aws/                    infra.sh, scaler-policy.json, ami-ci-policy.json, legacy-down.sh
    gvisor-patches/         as in jarvis3 (the DE0CH/gvisor fork's patch notes)
  flake.nix                 the service images (pigeon, magpie, hermit, herald, sandboxd, aviary-testing), egress
                            and hermit-jarvis, built by Nix (5.2)
  images/
    sandbox/                Dockerfile, apt-wrapper.sh, docker-wrapper.sh, reportlab-cjk.py, 90sandbox,
                            sudoers.d/sandbox, profile.d/00-sandbox-path.sh, smoke.sh (hermit spec 17,
                            supervisor spec 3 and 22)
  testing/
    fakes/                  lib aviary-fakes: in-process fake-magpie, fake-hermit, fake-supervisor for unit and
                            integration tests of the other crates (each implements its *-api crate's server trait)
    mock-model/             bin: scripted Anthropic Messages API + models route (7.5)
    fake-tools/             bin: a minimal MCP server (7.6)
    test-gateway/           bin: the reference sandbox gateway (7.3); also the gateway conformance oracle
    fake-sandboxd/          bin: sandboxd's API backed by Kubernetes Pods of the real sandbox image (7.2)
    fake-access/            bin: Cloudflare Access issuer: JWKS, get-identity, token minting (7.4)
    conformance/            bin aviary-conformance: suites hermit, gateway, model (section 8)
    e2e/                    the harness: k3d config, manifests, fixtures, the Rust driver (crate aviary-e2e)
    paseo-contract/         Node harness validating every Paseo frame (pigeon spec 18.1); package.json pins
                            DE0CH/claudecodeui paseo packages at a commit; lockfile committed
  docs/
    CONTRACTS.md            the conventions of section 3 and the endpoint names, for implementers of another
                            hermit or gateway
    HARNESS.md              running section 7 locally
  .github/workflows/        ci.yml, nightly.yml (section 6)
```

### 4.2 Workspace manifest

```toml
[workspace]
resolver = "3"
members = ["crates/*", "testing/fakes", "testing/mock-model", "testing/fake-tools", "testing/test-gateway",
           "testing/fake-sandboxd", "testing/fake-access", "testing/conformance", "testing/e2e"]

[workspace.package]
edition = "2024"
license = "AGPL-3.0-or-later"
repository = "https://github.com/DE0CH/aviary"
publish = false

[workspace.lints.rust]
unsafe_code = "deny"          # crates/supervisor (FUSE, fd passing) and crates/sandboxd allow it per module with
                              # #[allow(unsafe_code)] and a // SAFETY: comment on every block
missing_debug_implementations = "warn"

[workspace.lints.clippy]
all = { level = "deny", priority = -1 }
pedantic = { level = "warn", priority = -1 }
unwrap_used = "deny"          # tests may allow it
expect_used = "warn"
dbg_macro = "deny"
print_stdout = "deny"         # logs go through tracing; CLIs use a dedicated writer

[profile.release]
lto = "thin"
codegen-units = 1
strip = "symbols"
panic = "abort"               # a panic ends the process; Kubernetes or the keeper restarts it
```

`[workspace.dependencies]` pins exact versions once (each crate writes `tokio.workspace = true`): tokio (full),
axum 0.8, hyper 1, tower, tower-http, reqwest (rustls-tls-native-roots, stream, json; no default features), rustls,
serde, serde_json, tokio-postgres, deadpool-postgres, postgres-types, uuid (v4, v7, serde), time (serde, formatting),
chrono-tz (hermit ops), tracing, tracing-subscriber (json, env-filter off: levels are compiled-in defaults),
ring, subtle, base64, sha2, moka, aws-config, aws-sdk-s3, aws-sdk-ec2, kube, k8s-openapi, image, fast_image_resize,
libheif-rs, webp, kamadak-exif, multer, jsonwebtoken, x25519-dalek, rustix, nix, thiserror, anyhow (bins only),
clap (derive), proptest (dev), insta (dev, golden JSON), testcontainers is **not** used (CI provides services; local
runs use the harness).

### 4.3 The runtime crates

`aviary-core` (every service binary uses it; nothing in it knows a deployment):
- `env`: `required(name) -> String`, `required_list(name) -> Vec<String>` (comma list, trimmed, non-empty),
  `field(name)` for downward-API values; a missing or empty value aborts start with `{"msg":"missing env","name":…}`.
- `log`: tracing subscriber writing one JSON object per line to stdout (15.1), fixed field names, level `info` (no
  `RUST_LOG` switch: verbosity is not deployment configuration; debug logs are compiled in only for tests). Every
  string value except the typed id fields passes through `log::redact()` inside the subscriber, so no call site can
  forget it (rules and tests in 15.1).
- `ops`: the 9090 server (3.2) with `Readiness` (named gates set by the service, plus draining) and the metrics
  registry (`prometheus-client`), including `<svc>_build_info{version,revision}` = 1 and `<svc>_draining`.
- `drain`: SIGTERM/SIGINT handling, ordered async drain hooks with per-hook deadlines, exit codes.
- `auth`: `Callers` (3.7) as an axum extractor: `Caller::Pigeon` etc., route guards declared per router.
- `http`: the error envelope type, `Aviary-*` header names and extractors, request-id middleware, body limits,
  `no-store` default, a `reqwest::Client` builder (pool size, timeouts, `User-Agent: <svc>/<sha12>`,
  `Aviary-Protocol`), retry helper with jittered backoff for idempotent calls.
- `version`: the build facts (`env!` at compile time: `AVIARY_REVISION`, set by CI, `"dev"` locally).

`aviary-pg`:
- `connect(url, app_name, pool_size, statement_timeout)` → `deadpool_postgres::Pool` (rustls, `sslmode=require`
  without verification, keepalives 60/10/6 matching the server's).
- `migrate(pool, &[Migration])`: embedded SQL files `migrations/NNNN_name.sql`, applied in order inside one
  transaction each under `pg_advisory_lock(<service id>, 1)` on a dedicated connection, recorded in
  `_migrations(version int primary key, name text, sha256 text, applied_at timestamptz)`. A recorded migration whose
  sha256 differs from the embedded one is a start failure (someone edited history). Migrations are expand-only within
  a release (new tables, nullable columns, new indexes `CONCURRENTLY` outside the transaction); a contraction
  (drop) ships in a later release once no running version reads the object.
- `Leader`: `pg_try_advisory_lock(<key>)` on a dedicated connection, retried every 5 s, released on drain or
  connection loss; `is_leader()` watch channel. Keys are per service and live in its own database, so they cannot
  collide: hermit `(7300, <objid>)` (hermit 10.3), magpie `hashtext('magpie.<loop>')` (magpie 4, 15), the importer
  `hashtext('magpie-import.leader')`; egress and tools keep jarvis3's own.
- `Listener`: one dedicated LISTEN connection with reconnect (0.5 s doubling to 5 s) and a "wake everyone" on
  reconnect (magpie spec 14).
- Rule (every service): **no transaction is held across a network call** (another service, sandboxd, EC2, the bucket,
  the model). Long operations are protected by row claims renewed every 5 s (hermit 10.3, magpie's driver claims),
  never by a transaction-scoped advisory lock. `aviary-pg::Tx` has no `.await` on a non-database future in its API
  (a clippy `disallowed_methods` entry forbids `reqwest` calls inside the closure of `aviary_pg::tx`), so the role
  setting `idle_in_transaction_session_timeout = '60s'` (11.1) is a backstop that never fires in normal work.

### 4.4 Contract crates

Each `<service>-api` crate holds: the request/response/event types; the route table as constants
(`pub const CONVERSATIONS: &str = "/v1/conversations"` …) so servers and clients cannot drift; `PROTOCOL` numbers;
a typed `Client` (reqwest) used by every caller; a `Server` trait the fakes implement; and golden fixtures
(`fixtures/*.json`) that the server's integration tests and the client's tests both round-trip. A server crate depends
on its own `-api` crate and implements its routes with those types only.

### 4.5 Rules enforced in CI (source level)

- No `std::env::var` outside `aviary-core::env` (a clippy `disallowed_methods` entry), so no service can read a
  configuration variable that is not a declared secret or binding.
- No crate under `crates/` except `hermit` depends on `aws-sdk-ec2`, `kube`, or names `yyc`, `juicefs`, `openrouter`,
  `gvisor` (a `cargo deny` ban list for the dependency part, a grep test for the names, run on `crates/pigeon`,
  `crates/magpie`, `crates/supervisor`, `crates/*-api`). YYC-specific logic stays in hermit (and in egress and tools,
  which live in jarvis3).
- In `crates/pigeon` the literals `jarvis` and `yyc` appear only in `src/paseo/compat.rs` (the shipped apps' frozen
  wire constants, pigeon 1.2); a grep test enforces it.
- `cargo deny check` and `cargo audit` (advisories) fail the build.

### 4.6 Licence

AGPL-3.0-or-later (decision 3, Deyao 2026-11-02): pigeon ports CloudCLI-derived code (the tool-call mapper and
timeline projection of DE0CH/claudecodeui, AGPL-3.0) and speaks to the Paseo apps built from that fork, so the derived
parts are AGPL in any case; one licence for the whole repo keeps it simple. `LICENSE` is the AGPL-3.0 text; every
crate's `license` field and the image label say `AGPL-3.0-or-later`; `deny.toml`'s allowlist admits only licences
compatible with AGPL-3.0-or-later. jarvis3 has no licence file today; `crates/sandboxd` and `fabric/` moved from it
are Deyao's own code and take the repo's licence.

### 4.7 No secrets in the repo

The repo is public. It holds no keys, no tokens, no store layouts and no private names beyond what DE0CH/YYC already
publishes (YYC's own values live in DE0CH/YYC, not here). The harness generates its PKI, tokens and Access keys at run
time (`testing/e2e-harness`, output in the runner's temp dir, never committed). A CI job runs `gitleaks detect` on every
push.

---

## 5. Builds, images, releases

### 5.1 Binaries

| Binary | Crate | Target | Linking |
|---|---|---|---|
| `pigeon` | crates/pigeon | x86_64-unknown-linux-musl | static |
| `hermit` | crates/hermit | x86_64-unknown-linux-musl | static |
| `supervisor`, `supervisor-keeper` | crates/supervisor | x86_64-unknown-linux-musl | static |
| `sandboxd` | crates/sandboxd | x86_64-unknown-linux-musl | static |
| `magpie` | crates/magpie | x86_64-unknown-linux-gnu, built in `debian:bookworm` with `libheif-dev` | dynamic (glibc, libheif1) |
| `mock-model`, `fake-tools`, `test-gateway`, `fake-sandboxd`, `fake-access`, `aviary-conformance`, `aviary-e2e` | testing/* | musl | static |

`AVIARY_REVISION=<40 hex>` is set by CI for every build; builds are reproducible for a given toolchain and lockfile
(`SOURCE_DATE_EPOCH` = the commit time, `--remap-path-prefix`), so an unchanged crate graph gives a byte-identical
binary.

### 5.2 Images

The service images are built by Nix (`flake.nix`, `nix build --impure .#<name>-image` with `AVIARY_REVISION` = the
part's commit), for `linux/amd64` and `linux/arm64`, each natively on its own architecture; each is a pure function of
the source, the lock files and nixpkgs, so a build is a lookup in the Nix cache (6.1). hermit-sandbox is built by
BuildKit from `images/sandbox/Dockerfile` and the `dist/` of 5.1, `linux/amd64` only, `SOURCE_DATE_EPOCH` = commit time
and `rewrite-timestamp=true`.

| Image | Base | Content | User | Notes |
|---|---|---|---|---|
| `ghcr.io/de0ch/pigeon:<sha12>` | distroless-like root (CA certificates, time zones, `/etc/passwd`) | `/usr/local/bin/pigeon` | 65532 | the app bundle is a separate image (pigeon spec 17.1) |
| `ghcr.io/de0ch/magpie:<sha12>` | the same, with glibc and nixpkgs' libheif (HEVC decoder) | `/usr/local/bin/magpie` | 10001 | |
| `ghcr.io/de0ch/hermit:<sha12>` | the same | `hermit`, `juicefs` 1.4.1 CLI (sha256-checked) | 65532 | |
| `ghcr.io/de0ch/herald:<sha12>` | the same | `/usr/local/bin/herald` | 65532 | |
| `ghcr.io/de0ch/sandboxd:<sha12>` | the same, with iproute2, nftables, wireguard-tools, procps, tar, zstd, crane, bash | `/usr/local/bin/sandboxd` | root | the gateway pod's image (sandboxd as its main process) |
| `ghcr.io/de0ch/hermit-sandbox:<sha12>` | `node:22-trixie-slim` (by digest) | hermit spec 17 + supervisor spec 3: tools, Python venv, fonts, Docker, Claude Code at `/opt/claude-code`, `supervisor`, `supervisor-keeper`, `sandbox-secret` symlink | ENTRYPOINT keeper (PID 1, R12) | the slow layers are cached; the binaries are the last layer |
| test images `ghcr.io/de0ch/aviary-testing:<sha12>` | distroless-like | every `testing/*` binary on PATH, no entrypoint (the command names the binary) | 65532 | used by the harness and by jarvis3's conformance job |

### 5.3 Labels (every image)

```
org.opencontainers.image.source   = https://github.com/DE0CH/aviary
org.opencontainers.image.revision = <40 hex>
org.opencontainers.image.version  = <sha12>
org.opencontainers.image.licenses = AGPL-3.0-or-later
dev.de0ch.aviary.provides         = {"magpie-person":1,"magpie-sandbox":2,…}   (servers: what they serve)
dev.de0ch.aviary.accepts          = {"magpie-sandbox":[1,2],…}                 (servers: what they accept)
dev.de0ch.aviary.speaks           = {"magpie-sandbox":2,"hermit-sandbox":1,"supervisor-control":1}
                                    (hermit-sandbox only: the supervisor's client and server numbers)
dev.de0ch.aviary.supervisor       = sha256 of the supervisor binary (hermit-sandbox only; says whether a new
                                    sandbox image actually changed the supervisor)
```

### 5.4 Release

On `main`, after every gate of 6.1 passed: GitHub release `aviary-<sha12>` (target commitish = the commit, marked
"latest" = false; prerelease = false) with assets `pigeon`, `hermit`, `supervisor`, `supervisor-keeper`, `sandboxd`,
`aviary-conformance`, `pigeon-frames.ndjson` and `paseo-contract.tgz` (pigeon 18.1, 17.1: the frame corpus and the
contract harness the app's CI checks against), `SHA256SUMS`; the release body is the commit subject and the image digests. Consumers:
`fabric/host/fetch-sandboxd.sh aviary-<sha12>` (checks SHA256SUMS), the AMI bake (13.5), jarvis3's conformance job
(section 8), any other deployment's sandbox image.

### 5.5 Package visibility

New GHCR packages start private. The first push of each (`pigeon`, `magpie`, `hermit`, `sandboxd`, `hermit-sandbox`,
`aviary-testing`; and `pigeon-app` from claudecodeui) is followed by one `github-web` step making it public, done by
Claude in the task that first publishes it (no Deyao step). The YYC check (13.3) fetches each pinned tag anonymously,
so a package left private fails `yyc-ai.yml`'s `check` job, so `deploy` never applies the pin, not as
`ImagePullBackOff` in the cluster.

---

## 6. CI workflows (aviary)

### 6.1 `ci.yml` (every push and pull request)

```
on: push (all branches), pull_request, workflow_dispatch
concurrency: group ci-${{ github.ref }}; cancel-in-progress: ${{ github.ref != 'refs/heads/main' }}
permissions: contents read (jobs that publish ask for packages: write, contents: write, attestations: write,
             id-token: write on main only)
```

| Job | Needs | Runs | Time budget |
|---|---|---|---|
| `lint` | — | `cargo fmt --check`; `cargo clippy --workspace --all-targets --locked -- -D warnings`; `cargo deny check`; `gitleaks detect`; the source rules of 4.5; `shellcheck fabric/**/*.sh images/sandbox/*.sh` | 6 min |
| `test` | — | `cargo nextest run --workspace --locked` with service containers `ghcr.io/cloudnative-pg/postgresql:17.11-standard-trixie` (the production version) and MinIO (pinned by digest); env `AVIARY_TEST_DATABASE_URL`, `AVIARY_TEST_S3_*` (read only by test code). Covers every crate's unit and integration tests (magpie 20, hermit 24.2, pigeon 18.1-18.2 including the Node Paseo contract harness, supervisor 22's CI part with the fake `claude`) | 20 min |
| `sandboxd-soak` | — | the 45 s soak of hermit spec 14.6 (two daemons with fake runsc/ip/nft/wg, hostile clients, RSS flat under budget) | 5 min |
| `build` | — | 5.1: musl builds on the runner (rust-lld, no musl-gcc needed), magpie in a `debian:bookworm` job container; uploads artifact `dist` (binaries + SHA256SUMS) | 15 min |
| `images` | build | builds every image of 5.2 on an amd64 and an arm64 runner (the service images by Nix, from the Nix cache: bucket `de0ch-aviary-nix-cache`, signed by `aviary-nix-cache-1`, where this job adds what it built; hermit-sandbox by BuildKit with a GHA layer cache), runs each image's checks, and exports each as a tar artifact | 15 min (a cold Nix cache 90) |
| `sandbox-smoke` | images | `images/sandbox/smoke.sh` against the built sandbox image under runc: hermit spec 17's list plus supervisor spec 22's image checks (keeper refuses to start without its token with the stated message; `claude` resolves to `/opt/claude-code/bin/claude`, root-owned; a test gateway sees Claude Code's model and MCP calls as proxied `http://model/…`, `http://tools/mcp`) | 8 min |
| `gvisor` | images | installs `runsc` from DE0CH/gvisor release `precopy-20261021.2` (sha256-checked) as a Docker runtime and runs the sandbox image under it with `/dev/fuse`: the supervisor mounts `~/.claude`, a scripted turn against mock-model writes its transcript through FUSE into a test magpie, bytes compared (supervisor spec 22 items 1, 2, 4, 5 that need no AWS host) | 12 min |
| `e2e` | images | the harness, section 7 (all scenarios in 7.8) | 25 min |
| `e2e-chaos` | images | the harness for 60 min with two drivers sending turns continuously while a chaos loop rolls pigeon, magpie, hermit and the gateway in random order every 2-5 min and kills a random supervisor every 10 min (seeded; the seed is printed and `chaos:<minutes>:<seed>` repeats the schedule); gate: every turn ends `completed`, every row seen exactly once by every socket, every send accepted within 180 s. On every push (Deyao, 2026-11-25) | 75 min |
| `publish` | lint, test, sandboxd-soak, sandbox-smoke, gvisor, e2e, e2e-chaos; **main only** | pushes every image tar as `<name>:<sha12>` (never `latest`), `actions/attest-build-provenance` for each digest, then the release of 5.4; writes the digests to the job summary and says whether `dev.de0ch.aviary.supervisor` changed against the previous main commit's sandbox image | 5 min |

Never cancelled on `main` (a half-published set would leave a tag without its release). A failed gate means nothing of
that commit is published: every tag that exists has passed the harness, so YYC can pin any existing tag.

### 6.2 `nightly.yml` (cron 18:00 UTC = 02:00 HKT, and `workflow_dispatch`)

| Job | What | Bound |
|---|---|---|
| `supervisor-soak` | supervisor spec 22's soak, run for 5 h (the hosted-runner limit is 6 h): synthetic turns with 1 MiB lines, random supervisor kills, fake-magpie outages of 1 s to 10 min, a full scratch disk; gates RSS flat under 64 MiB, no lost or duplicated transcript bytes, no hung FUSE request | 5 h 30 |
| `deps` | `cargo update --dry-run` report and `cargo deny check advisories` against today's database; opens nothing automatically, writes the summary | 5 min |

Public repository: hosted-runner minutes are free; no self-hosted runner is needed.

### 6.3 Toolchain and caches

`dtolnay/rust-toolchain` is not used; the runner installs exactly `rust-toolchain.toml`'s version with `rustup`.
`Swatinem/rust-cache` keyed by the lockfile and the toolchain. Action versions pinned by commit SHA (public repo:
supply-chain hygiene), refreshed by the `deps` job's report.

---

## 7. Integration harness (all services + Postgres + fake sandboxes in CI)

Purpose: run the real turn path (app protocol → pigeon → magpie → hermit → supervisor in the real sandbox image →
Claude Code → gateway → model) on one GitHub runner, with real Postgres, a real S3, real JuiceFS for `/workspace`, real
NetworkPolicies and real rollouts, no cloud account and no model cost. The same harness runs on a laptop
(`testing/e2e-harness`, Docker + k3d).

### 7.1 Topology

```
runner host
  docker: postgres:17.11 (CNPG image)  :5432    databases magpie, hermit, jfs (roles of the same names)
  docker: minio                       :9000    buckets magpie, jfs
  juicefs format + mount (host FUSE)  /mnt/aviary-jfs   (meta = postgres jfs, data = minio jfs)
  k3d cluster "aviary-e2e": rancher/k3s:v1.36.5-k3s1 (YYC's version), 1 server + 2 agents, traefik off,
      --volume /mnt/aviary-jfs:/mnt/aviary-jfs@all (shared propagation), host reachable as host.k3d.internal
    namespace aviary-e2e (NetworkPolicies of 7.7 enforced by k3s's controller, as in YYC)
      pigeon x2         Service pigeon:80        the image under test, env = secrets only
      magpie x2         Service magpie:80
      hermit x2         Service hermit:80        hermit.json = testing/e2e/hermit.json (7.2), catalogue fixture
      test-gateway x2   Services model:80, egress:80 (8082), and a fixed ClusterIP 10.43.200.10 for :3128 (the VIP)
      fake-tools x1     Service tools:80
      mock-model x1     Service mock-model:80    reached only by the gateway
      fake-access x1    Service access:443       TLS with the harness CA
      fake-sandboxd x1  Service sandboxd:7443    mTLS with the harness PKI; creates sandbox Pods
      sandbox Pods      label app=sandbox, image hermit-sandbox:<built>, privileged (FUSE), hostPath
                        /mnt/aviary-jfs/ws/<id> → /workspace
      driver Job        aviary-e2e (Rust) + paseo-contract (Node)
```

Everything is created by `e2e-harness up` from `testing/e2e/k8s/` (plain YAML plus a generated
`secrets.yaml`), images imported with `k3d image import`. Tokens, PKI (`fabric/host/make-certs.sh` into the temp dir)
and the Access key pair are generated per run.

### 7.2 The fake sandbox: `fake-sandboxd`

hermit's YYC fabric talks sandboxd's API over mTLS (hermit spec 13.15). `fake-sandboxd` implements that API from
`sandboxd-api` with Kubernetes Pods instead of gVisor:

| sandboxd route | fake-sandboxd |
|---|---|
| `GET /v1/host` | `{name: "fake-gw", kind: "gateway", cpus: 8, memTotal: 16 GiB, memAvailable: …, gvisor: "fake", sandboxd: "<sha12>"}` |
| `PUT /v1/peers`, `PUT /v1/egress` | 204, recorded (asserted by tests) |
| `POST /v1/images` | 204 (the image is pre-imported) |
| `POST /v1/sandboxes` (spec: name, image, env, args, mounts, ip, memory, cpu) | creates Pod `sbx-<name>` with the image, args, env, `resources.limits` = memory/cpu, the `/workspace` hostPath for the JuiceFS `ws/<id>` path hermit named, `securityContext.privileged: true`, label `app=sandbox`, `hermit/sandbox=<name>`; answers when the Pod is Running with its Pod IP as the sandbox address |
| `GET /v1/sandboxes`, `GET …/spec` | from the Pods |
| `DELETE /v1/sandboxes/{n}` | deletes the Pod (grace 25 s: the keeper's 20 s) |
| `POST …/send`, `POST`/`GET …/receive` (migration) | 501 `not_supported` |

hermit's harness config gives one gateway host with 6 slots and `maxHosts: 0`, so the scaler never launches EC2 and
packing never migrates; EC2 and migration paths are covered by hermit's own integration tests (fake EC2, fake
sandboxd) and by the staging acceptance on real hosts (hermit spec 24.3). This is a deployment of hermit's YYC
implementation with different values in its own config file, not a switch: hermit's code paths are the same.

`testing/e2e/hermit.json` differs from YYC's only in values: `signIn.access` → the fake issuer
(`teamDomain: access.aviary-e2e.svc.cluster.local`), `sandbox.image` → the built image, `idleMinutes: 2`,
`maxLifetimeHours: 1`, `fabric.gateway.url` → `https://sandboxd.aviary-e2e.svc.cluster.local:7443`, CIDRs → the k3d pod
range, `egressVip` → 10.43.200.10, `aws.maxHosts: 0`, `ops.deployments` → the harness Deployments.

### 7.3 `test-gateway` (the reference gateway)

Implements the gateway contract of 3.11 and nothing YYC-specific:
- `:3128` the proxy (3.11.1): credential and verification exactly as 3.11.2 (signed tokens checked locally with
  hermit's published keys, liveness from hermit cached 30 s and served stale while hermit fails); the routes,
  headers, limits and drain of 3.11.3-3.11.6, with `model` → `http://mock-model`. `CONNECT` and any host that is not
  a fixed name → 403 `{"code":"not_allowed"}` (the harness has no internet and no self-host paths).
- `:8082` in-cluster API: `GET /v1/aviary/models` (forwarded to mock-model) for pigeon's bearer, `GET
  /v1/ops/summary` (a fixture in the shape of 15.3) for hermit's bearer.

### 7.4 `fake-access`

TLS on 443 with a certificate for `access.aviary-e2e.svc.cluster.local` from the harness CA; pigeon's Pods get the CA
through `SSL_CERT_FILE=/etc/aviary-e2e/ca.crt` (a mounted harness Secret; the only harness-only env, and it changes
trust, not behaviour). Routes: `GET /cdn-cgi/access/certs` (JWKS, RS256, two keys, rotation test), `GET
/cdn-cgi/access/get-identity` (cookie `CF_Authorization` → `{"email","name","given_name"}`), `POST /__mint`
`{"email","name","aud","expSeconds","kid"?}` → `{"token"}` for the driver. Audience = the harness `hermit.json` value.

### 7.5 `mock-model`

Anthropic Messages API compatible enough for Claude Code 2.1.304 (streaming SSE: `message_start`,
`content_block_start/delta/stop`, `message_delta`, `message_stop`; non-streaming JSON; `count_tokens` answering a
fixed estimate), plus the models route of R18. Behaviour is scripted by the driver:

```
POST /__script   {"match": {"conversation": "<uuid>"?, "textContains": "<s>"?, "nth": 1?},
                  "reply": [ {"text": "…", "chunkMs": 20},
                             {"thinking": "…"},
                             {"toolUse": {"name": "Bash", "input": {"command": "echo hi > /workspace/a.txt"}}},
                             {"sleepMs": 60000},
                             {"error": {"status": 529, "type": "overloaded_error"}} ],
                  "times": 1}
GET  /__requests?conversation=<uuid>   -> [{"at", "model", "system": "<sha256>", "messages": n,
                                            "images": [{"width","height","bytes"}], "tools": ["mcp__tools__…"]}]
POST /__reset
```

Unmatched requests answer a short text reply (`"ok"`), and title requests (the title model id from the models route)
answer `"測試標題"`. The conversation is known from the `Aviary-Sandbox-Conversation` header the gateway adds.

### 7.6 `fake-tools`

A REST API (decision 15: `GET /v1/tools`, `POST /v1/tools/<name>`) with two tools (`echo`, `slow_echo` with a delay
argument) and the gateway pair token check; records calls with `Aviary-Sandbox-Person` for assertions.

### 7.7 Harness NetworkPolicies

The same shape as YYC's (section 10), with harness labels, so a policy mistake in the shape fails here first:
default deny ingress and egress; pigeon ← driver; magpie ← pigeon, hermit, test-gateway; hermit ← pigeon, magpie,
test-gateway, fake-tools, driver (admin); sandbox Pods → test-gateway 3128 only (no DNS: the proxy is addressed by
IP, as in YYC); hermit → sandbox Pods 7070, fake-sandboxd 7443; the gateway → magpie, hermit, mock-model, fake-tools;
pigeon → magpie, hermit, model (test-gateway 8082), fake-access 443; everyone → kube-dns; hermit → the API server
(read Role as in YYC) and host.k3d.internal:5432; magpie → host.k3d.internal:5432 and :9000.

### 7.8 Scenarios (the `e2e` gate; each asserts its outcome, all run in one job)

The driver speaks Paseo over `ws://pigeon/paseo-ws` with `cf-access-token` from fake-access, validates every frame
with the Node contract harness, and reads magpie's admin routes to cross-check state.

| # | Scenario | Pass when |
|---|---|---|
| E1 | Sign-in: allowed person, `/api/jarvis/me`; not-allowed person; expired token; second JWKS key | 200 with the person; 403 page; 401; 200 after rotation |
| E2 | New conversation, one text turn (mock streams 40 chunks) | the reply arrives as whole rows with dense `rowSeq` and zero catch-up requests (no live frames in this release, R36); `turn_completed` after the last row; title replaced by `測試標題` |
| E3 | Tool turn (Bash writes `/workspace/a.txt`), then the files panel and a download | the file is listed and downloads byte-equal; the JuiceFS path `ws/<id>/a.txt` exists on the host mount |
| E4 | SendUserFile of a 5 MiB file | the sent-file snapshot downloads byte-equal after the workspace file is deleted |
| E5 | Image attachment (6000×4000 JPEG, EXIF rotated) | mock-model's recorded request has one image ≤ 2000 px long edge and ≤ 3,932,160 bytes; the timeline thumbnail loads through `/api/blobs/…` with a valid signature, 404 with a bad one |
| E6 | History: 120 turns preloaded through the driver (fast mock), then `tail`, `before` paging | pages of 40, no gap, no duplicate; cost per page flat (time per page within 2× of the first) |
| E7 | Reconnect mid-turn: close the socket at a random frame, reopen | the row set after resume equals the uninterrupted run's |
| E8 | Stop during a 60 s streaming turn | `canceled` within 30 s; the next turn works |
| E9 | Rollouts during a 90 s turn: `kubectl rollout restart` of pigeon, then magpie, then hermit, then test-gateway, waiting for each | the turn completes; every socket saw every row exactly once; no request the driver made got anything but success or `unavailable`+`retry-after` |
| E10 | `kill -9` of the supervisor inside the sandbox Pod mid-turn | the turn completes; magpie holds the transcript bytes exactly once (reconstruct sha256 = the Pod-side copy) |
| E11 | Delete the sandbox Pod mid-turn | turn failed `sandbox.lost` within 40 s; the next turn starts a new sandbox and resumes history |
| E12 | Idle release (2 min), then a new turn | a new sandbox; Claude Code resumes from magpie's `~/.claude`; the model sees the earlier messages |
| E13 | Shares: create, view as another person, continue | the viewer sees rows up to the share point; continue creates a conversation owned by the viewer with the frozen `/workspace` |
| E14 | Delete, restore, delete for good | list changes on every socket; after delete for good magpie 404s and hermit has released the sandbox and deleted the workspace |
| E15 | View as (admin) | pigeon spec 18.4's view-as checks, run against the harness |
| E16 | Ops: `/ops/` through pigeon as admin; `/ops/snapshot` with the monitor token | 200; both monitor contracts of hermit 18.3 hold: `ai_slim`'s field paths and types, and `ai_activity`'s paths and values (`starts.list[].what` only `start`/`release`, `owner` an email, every `at` RFC 3339 with three fraction digits and `Z`, `events` ≤ 60, `egress.recent[].owner` an email); non-admin 403 |
| E17 | Busy: 7 conversations start at once (6 slots) | 6 run; the 7th shows the sandbox-busy state and starts once a slot frees (no 500) |
| E18 | Two replicas: every pigeon RPC sent alternately through both pigeon Pods (the driver connects to Pod IPs directly) | identical results |
| E19 | NetworkPolicy probes: from a sandbox Pod `curl http://magpie/…` without the proxy; to the internet; to hermit 8080 directly | all fail to connect |
| E20 | Drain timing: SIGTERM each service Pod with a live socket/long-poll | each exits within its grace; clients resume on the other replica |
| E21 | Reconnect mid-turn across a compaction: close the socket while the mock's turn makes Claude Code compact (rename-over of the transcript), reopen | no gap loop (at most one catch-up per reconnect), no duplicate row, the epoch unchanged when no visible row changed (magpie 7.3 rule 9) |
| E22 | Interleaved non-row events: a turn that produces `title.changed`, `task.status`, `sandbox.*` and a `retract` between rows | every socket receives every row once with zero refetches; the retract that removed a visible row arrives as one `agent.timeline.replacement` |
| E23 | Gateway drain: during a mock-model stream of 60 s and a 25 s magpie long-poll, SIGTERM the test-gateway Pod carrying both | the stream completes unbroken; the long-poll answers normally; the supervisor's next call lands on the other replica (`Connection: close` seen); `drain_cut_total` 0 |
| E24 | hermit unavailable: scale hermit to 0 for 3 min during a turn | the turn completes (model, tools and FUSE writes keep flowing through the gateway on cached liveness); no 401/403/407 anywhere |

Artifacts on failure: every Pod's logs, `kubectl get events`, magpie and hermit admin dumps, mock-model's request log,
the driver's frame log (no secrets: the harness generates throwaway ones).

---

## 8. Conformance suites (contracts other repos implement)

`aviary-conformance` (in the release and the testing image) checks an implementation of a contract from the outside.
It brings its own fakes for whatever the implementation calls, served on one listener on port 80 that answers by
`Host` header (so the implementation reaches them at the fixed names through Docker network aliases or Services).

```
aviary-conformance hermit  --target http://hermit --tokens tokens.json [--capabilities pause,snapshot]
aviary-conformance gateway --proxy http://gw:3128 --api http://gw:8082 --sandbox-ip <source address>
                           --serve-fakes :80 --tokens tokens.json --signing-key <harness hermit key>
                           --sigterm '<command that sends SIGTERM to the replica under test>'
aviary-conformance model   --target http://model --token <pigeon pair token>
exit code 0 = pass; JUnit XML to --junit <file>; one line per check on stdout
```

### 8.1 `hermit`

hermit spec 24.1 (every route's shapes and errors, idempotent `PUT` from two clients yields one sandbox, long-poll
wake and timeout, feed order and `latest` and 410, release idempotent, pause/resume/snapshot either work or 501
consistently with `capabilities`, `/v1/sandbox/self` only with the gateway token and matching headers, secrets 404
`not_granted`, token lookup 404 for gone sandboxes, people rules). Run by aviary's CI against hermit with a fake fabric
(in `test`) and by any other hermit implementation's CI (Jarvis 4, open).

### 8.2 `gateway`

| Check | Expect |
|---|---|
| no proxy credential / bad signature / expired / wrong source address / unknown kid after one refetch | 407 / 403 / 403 / 403 / 403 |
| the fake hermit answers `live: false` | 403 within 30 s of the change |
| the fake hermit down (connection refused, then 5 s timeouts, then 503) for 3 min, with a token seen before and one never seen | both admitted on every request; never 401/403/407/5xx caused by hermit (3.11.2 step 3) |
| `http://magpie/v1/sandbox/x` | reaches the fake magpie with `Aviary-Sandbox-Id/Conversation/Person` from the verified token (`-Person` lowercased), the gateway's magpie pair bearer, no client `Authorization`/`x-api-key`, no client `Aviary-*` |
| `http://magpie/v1/conversations` (non-sandbox path) | 403 |
| `http://hermit/v1/sandbox/self` | reaches the fake hermit with the same headers; `http://hermit/v1/people` 403 |
| `http://model/v1/messages` streaming | the fake upstream sees the gateway's credential, never the sandbox's `x-api-key`/`authorization`; first SSE byte relayed within 200 ms of the upstream's; 30 s stream unbroken |
| `http://model/v1/aviary/models` (sandbox) and `:8082/v1/aviary/models` (pigeon bearer) | the R18 shape |
| `http://tools/v1/tools/<name>` | reaches the fake tools with the tools pair bearer and `Aviary-Sandbox-Person`; a fake tools answering after 170 s is relayed, not cut |
| 25 s long-poll to magpie | not cut |
| 64 MiB PUT to magpie | delivered; 64 MiB + 1 → 413 |
| 40 PUTs of 1 MiB to magpie on one kept-alive connection (40 MiB, above any per-connection upload cap) | all delivered (the upload cap counts only internet traffic, 3.11.4) |
| the VIP or a Service name of the gateway as the host of a proxy-form request | served as a self host (the suite checks only that it is not refused as a private address and not forwarded) |
| drain: `--sigterm` while a 60 s model SSE stream and a 25 s magpie long-poll are open on the replica, plus an idle kept-alive connection | the stream completes unbroken, the long-poll answers normally, the next request on the busy keep-alive connection carries `Connection: close`, the idle one is closed; a new connection to the replica is refused |
| `:8082/v1/ops/summary` with hermit's bearer | the 15.3 shape; without it 401 |
| internet: a host the implementation's test config denies | refused and logged (the implementation states its own allow rules; the suite only checks deny-by-default for an unlisted host) |

jarvis3's `chat.yml` runs this suite against the egress image under test (section 17.3).

### 8.3 `model`

The models route shape (R18), a Messages API round trip (non-streaming and streaming) relayed unchanged apart from
credentials, and `count_tokens` passthrough.

---
## 9. YYC deployment (DE0CH/YYC `yyc/deploy`)

### 9.1 Layout

```
yyc/deploy/
  kustomization.yaml        resources, image pins, ConfigMap generators, secrets-rev annotations (9.2)
  limits.yaml               LimitRange in yyc-ai (as jarvis3's limits.yaml, same numbers)
  network.yaml              the namespace-wide NetworkPolicies (10.1)
  cloudflared.yaml          Deployment, PDB, NetworkPolicies (tunnel "pigeon")
  pigeon.yaml               Deployment, Service, HPA, PDB, NetworkPolicies (9.4)
  magpie.yaml               Deployment, Service, HPA, PDB, NetworkPolicies (9.5)
  hermit/
    hermit.yaml             Deployment, Service, ServiceAccount, Role, RoleBinding, PDB, NetworkPolicies (9.6)
    fabric.yaml             hermit-gateway DaemonSet + ConfigMap (yyc-ai-fabric), hermit-overlay-route DaemonSet
    juicefs.yaml            static PV hermit-jfs + PVC yyc-ai/hermit-jfs (same JuiceFS volume yyc-ai)
    hermit.json             hermit's YYC configuration (hermit spec 9)
    system-prompt.md        moved from yyc/deploy/system-prompt.md
    sandbox-rules.md        hermit spec 9
  catalogue.json            who may use YYC AI and every YYC app, and admins (hermit spec 3.4.1); also read by tofu,
                            yyc-mcp (as ConfigMap yyc/yyc-catalogue, key config.json) and the mcp-image smoke test
  egress/
    egress.yaml             Deployment, Services egress + model, PDB, NetworkPolicies (9.7)
    egress.json             egress's own configuration (section 17.2); also read by the ZDR guard and openrouter.py (13.6)
  tools/
    tools.yaml              Deployment, Service, PDB, NetworkPolicies (9.7)
    tools.json              tools' own configuration (17.2)
  bridges.yaml              the new side of the cross-cluster bridges (25.3): ui-legacy, pigeon-bridge, hermit-bridge,
                            the placeholders yyc/yyc-mcp and yyc/web; until A6 / T4
  jobs/                     the switch's one-off Jobs (9.12); deleted from git after the teardown
  sealed/                   SealedSecrets (section 12), sealed for the new cluster, applied by the deploy workflow's
                            fenced step (impersonating yyc-deploy/sealed-secrets-applier)

yyc-ai/legacy/              the old stack, today's yyc/deploy/ files moved unchanged (kustomization.yaml with the
                            jarvis3 remote base and patches, ui.yaml, sandbox-vm.yaml, sandbox.yaml, ops.yaml, db.yaml,
                            the jarvis PV/PVC, config.json, system-prompt.md, sealed/, sealed-secrets-rbac.yaml), applied
                            by Flux Kustomizations yyc-ai / yyc-ai-secrets on the OLD cluster (25.2); gains the
                            window's old-side objects in A1; deleted in the teardown (18) except catalogue/ (the
                            yyc/yyc-catalogue generator) until yyc-mcp leaves the old cluster
```

Not in this folder (cluster-level objects of the new cluster, `news-stream/deploy/clusters/talos/`, applied by the
new cluster's own workflow (25.2; before decision 13 this paragraph said `clusters/hetzner/`, Flux `yyc-apps`), so a
`yyc-ai` commit can neither widen its own rights nor relabel a namespace; deploy-workflows 4.2): Namespaces `yyc-ai`
(PSS restricted), `yyc-ai-fabric` (PSS privileged: the gateway and overlay-route DaemonSets only) and, until A6,
`jarvis`; the deployer's RBAC and the fenced applier's RoleBindings (`yyc-ai-access.yaml`); the JuiceFS CSI driver's
HelmRelease (`juicefs-csi.yaml`, moved unchanged); the deploy path itself (`deploy-access.yaml`).

Moving today's files to `yyc-ai/legacy/` (WP-39, DESIGN 15.4: copy, identical render, Flux path switch with
`prune: false`, delete the old copy) renders the same objects, so Flux applies no change to the running old stack,
and `yyc/deploy/` holds only the new stack. The `yyc-catalogue` generator (namespace `yyc`) stays in `legacy/`
reading `config.json` until the switch commit (A3, 18 here), which moves it to the top level as
`files: [config.json=catalogue.json]` (R40): the ConfigMap's name, its key `config.json` and the path
`/etc/yyc-catalogue/config.json` never change, so neither Deployment `yyc-mcp` nor CronJob `mcp-cleanup`
(`news-stream/deploy/apps/yyc-mcp.yaml` lines 39 and 119, Kustomization `yyc-apps`) changes or rolls, and no order of
the `yyc-ai` deploy and Kustomization `yyc-apps` can leave a Pod without the file. The same commit moves every other reader and
text (hermit 21.5's table: tofu `access.tf`, `cloudflare.yml`'s path filter, `.github/workflows/mcp-image.yml:62`'s
mount source, `mcp-edge/test/catalogue.test.ts`, and the README and comment lines it lists). The teardown (cutover
P5.4) only deletes `config.json` with `legacy/`.

### 9.2 `kustomization.yaml` (top level, the parts that matter)

```yaml
apiVersion: kustomize.config.k8s.io/v1beta1
kind: Kustomization
resources:
  - limits.yaml
  - network.yaml
  - cloudflared.yaml
  - pigeon.yaml
  - magpie.yaml
  - hermit/hermit.yaml
  - hermit/fabric.yaml
  - hermit/juicefs.yaml
  - egress/egress.yaml
  - tools/tools.yaml
  - legacy                      # removed in the teardown commit
images:                          # one aviary commit for all three (checked by yyc-ai.yml check, 13.3)
  - {name: pigeon, newName: ghcr.io/de0ch/pigeon, newTag: "<sha12>"}
  - {name: magpie, newName: ghcr.io/de0ch/magpie, newTag: "<sha12>"}
  - {name: hermit, newName: ghcr.io/de0ch/hermit, newTag: "<sha12>"}
  - {name: sandboxd, newName: ghcr.io/de0ch/sandboxd, newTag: "<sha12>"}
  - {name: pigeon-app, newName: ghcr.io/de0ch/pigeon-app, newTag: "<claudecodeui sha12>"}
  - {name: jarvis3-chat, newName: ghcr.io/de0ch/jarvis3-chat, newTag: "<jarvis3 sha12>"}   # egress, tools
configMapGenerator:
  - {name: hermit-config, namespace: yyc-ai,
     files: [hermit/hermit.json, hermit/system-prompt.md, hermit/sandbox-rules.md]}        # hashed: a change rolls hermit
  - {name: catalogue, namespace: yyc-ai, files: [catalogue.json],
     options: {disableNameSuffixHash: true}}                                                # live reload, no roll
  - {name: yyc-catalogue, namespace: yyc, files: [config.json=catalogue.json],                # key stays config.json (R40)
     options: {disableNameSuffixHash: true}}                                                # from the switch commit on
  - {name: egress-config, namespace: yyc-ai, files: [egress/egress.json]}                   # hashed
  - {name: tools-config, namespace: yyc-ai, files: [tools/tools.json]}                      # hashed
patches:
  # the Secrets' revision, per Deployment (gap-2 2.15): bump only the reader of a resealed Secret
  - target: {kind: Deployment, name: pigeon}
    patch: '[{"op":"add","path":"/spec/template/metadata/annotations/yyc~1secrets-rev","value":"<date>-<why>"}]'
  # … one such patch per Deployment: magpie, hermit, egress, tools, cloudflared
```

The sandbox image is pinned in `hermit/hermit.json` (`sandbox.image`, by digest), with `fabric.sandboxd`
(`aviary-<sha12>`, the release the AMI and the gateway install) and `fabric.runsc` (the DE0CH/gvisor release); these
three are what the AMI follows (13.5). The `sandboxd` image tag above equals `fabric.sandboxd`'s sha12 (checked).

### 9.3 Every Deployment in `yyc-ai`

| | pigeon | magpie | hermit | egress | tools | cloudflared |
|---|---|---|---|---|---|---|
| replicas | HPA 2-6 (CPU 70 %) | HPA 2-6 (CPU 70 %) | 2 | 2 | 2 | 2 |
| strategy | RollingUpdate maxSurge 1, maxUnavailable 0 (all) | | | | | |
| minReadySeconds | 5 | 5 | 5 | 5 | 5 | 5 |
| readiness | `:9090/readyz` 2 s ×1 | `:9090/readyz` 3 s ×1 | `:9090/readyz` 3 s ×1 | `:9090/readyz` 3 s ×1 | `:9090/readyz` 3 s | `:2000/ready` 5 s |
| liveness | `:9090/healthz` 10 s ×3, delay 5 | 20 s ×6 | 10 s ×6 | 20 s ×3, delay 10 | 20 s ×3, delay 20 | 20 s ×6, delay 30 |
| preStop sleep | 5 s | 5 s | 10 s | 10 s | 10 s | — |
| drain bound | 15 s sockets, moving transfers up to 300 s (pigeon 11.2) | 30 s | 60 s (migration final phase) | 600 s model streams and tunnels, 60 s fixed names (3.11.6) | 150 s | 3 min (`--grace-period`) |
| terminationGracePeriodSeconds | 320 | 75 | 90 | 625 | 180 | 200 |
| PDB | minAvailable 1 (all) | | | | | |
| spread | topologySpreadConstraints hostname, maxSkew 1, `matchLabelKeys: [pod-template-hash]`, DoNotSchedule (all) | | | | | |
| node affinity | none | none | JuiceFS CSI nodes (cx33, cpx32), for the PVC | none | none | none |
| priorityClassName | `serving` (all) | | | | | |
| ServiceAccount token | no | no | yes (`hermit`, Role 9.11) | no | no | no |
| securityContext | runAsNonRoot, readOnlyRootFilesystem, drop ALL, no privilege escalation, seccomp RuntimeDefault, `enableServiceLinks: false` (all) | | | | | |
| uid | 65532 | 10001 | 65532 | 1000 | 1000 | 65532 |
| ConfigMaps mounted | none | none | `hermit-config`, `catalogue` | `egress-config` | `tools-config`, `catalogue` | none |
| volumes | app bundle emptyDir 512Mi (initContainer) | `/tmp` emptyDir 1Gi | `/tmp` 256Mi, PVC `hermit-jfs` at `/jfs` | cache emptyDir 6Gi, `/tmp` 256Mi | `/tmp` 256Mi | none |
| resources | 16.1 | | | | | |

### 9.4 `pigeon.yaml`

```yaml
apiVersion: apps/v1
kind: Deployment
metadata: {name: pigeon, namespace: yyc-ai, labels: {app: pigeon}}
spec:
  revisionHistoryLimit: 3
  minReadySeconds: 5
  selector: {matchLabels: {app: pigeon}}
  strategy: {type: RollingUpdate, rollingUpdate: {maxSurge: 1, maxUnavailable: 0}}
  template:
    metadata: {labels: {app: pigeon}}
    spec:
      automountServiceAccountToken: false
      enableServiceLinks: false
      priorityClassName: serving
      terminationGracePeriodSeconds: 320
      topologySpreadConstraints:
        - {maxSkew: 1, topologyKey: kubernetes.io/hostname, whenUnsatisfiable: DoNotSchedule,
           labelSelector: {matchLabels: {app: pigeon}}, matchLabelKeys: [pod-template-hash]}
      securityContext: {runAsNonRoot: true, runAsUser: 65532, runAsGroup: 65532, seccompProfile: {type: RuntimeDefault}}
      initContainers:
        - name: app
          image: pigeon-app
          command: ["/cp", "-a", "/app/.", "/srv/app/"]
          resources: {requests: {cpu: 50m, memory: 32Mi}, limits: {memory: 32Mi}}
          securityContext: {allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: {drop: [ALL]}}
          volumeMounts: [{name: app, mountPath: /srv/app}]
      containers:
        - name: pigeon
          image: pigeon
          args: [serve]
          ports: [{name: http, containerPort: 8080}, {name: ops, containerPort: 9090}]
          env:
            - {name: PIGEON_TOKEN_MAGPIE, valueFrom: {secretKeyRef: {name: pigeon, key: PIGEON_TOKEN_MAGPIE}}}
            - {name: PIGEON_TOKEN_HERMIT, valueFrom: {secretKeyRef: {name: pigeon, key: PIGEON_TOKEN_HERMIT}}}
            - {name: PIGEON_TOKEN_MODEL, valueFrom: {secretKeyRef: {name: pigeon, key: PIGEON_TOKEN_MODEL}}}
            - {name: PIGEON_VIEW_AS_KEYS, valueFrom: {secretKeyRef: {name: pigeon, key: PIGEON_VIEW_AS_KEYS}}}
            - {name: PIGEON_BLOB_KEYS, valueFrom: {secretKeyRef: {name: pigeon, key: PIGEON_BLOB_KEYS}}}
          readinessProbe: {httpGet: {path: /readyz, port: ops}, periodSeconds: 2, failureThreshold: 1}
          livenessProbe: {httpGet: {path: /healthz, port: ops}, initialDelaySeconds: 5, periodSeconds: 10, failureThreshold: 3}
          lifecycle: {preStop: {sleep: {seconds: 5}}}
          resources:
            requests: {cpu: 100m, memory: 384Mi, ephemeral-storage: 64Mi}
            limits: {cpu: "1", memory: 384Mi, ephemeral-storage: 128Mi}
          securityContext: {allowPrivilegeEscalation: false, readOnlyRootFilesystem: true, capabilities: {drop: [ALL]}}
          volumeMounts: [{name: app, mountPath: /srv/app, readOnly: true}]
      volumes:
        - {name: app, emptyDir: {sizeLimit: 512Mi}}
---
apiVersion: v1
kind: Service
metadata: {name: pigeon, namespace: yyc-ai}
spec: {selector: {app: pigeon}, ports: [{name: http, port: 80, targetPort: http}]}
---
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata: {name: pigeon, namespace: yyc-ai}
spec:
  scaleTargetRef: {apiVersion: apps/v1, kind: Deployment, name: pigeon}
  minReplicas: 2
  maxReplicas: 6
  metrics: [{type: Resource, resource: {name: cpu, target: {type: Utilization, averageUtilization: 70}}}]
  behavior: {scaleDown: {stabilizationWindowSeconds: 600, policies: [{type: Pods, value: 1, periodSeconds: 120}]}}
---
apiVersion: policy/v1
kind: PodDisruptionBudget
metadata: {name: pigeon, namespace: yyc-ai}
spec: {minAvailable: 1, selector: {matchLabels: {app: pigeon}}}
```

The Deployment has no `replicas` field (the HPA owns it, and the deploy's server-side apply must not own it). The HPA
is not pinned during the switch (DESIGN 9.3: a scale-in is an ordinary reconnect, and the drain closes request-free
sockets first). A slow scale
down (one Pod per 2 min after 10 min calm) keeps socket churn low: each removed Pod costs its tabs one reconnect.

### 9.5 `magpie.yaml`

As 9.4 with: `image: magpie`, `args: [serve]`, uid 10001, `terminationGracePeriodSeconds: 75`, readiness period 3 s,
liveness period 20 s failureThreshold 6, `/tmp` emptyDir 1Gi, resources 16.1, HPA 2-6 with the same behaviour, and:

```yaml
          env:
            - {name: MAGPIE_POD, valueFrom: {fieldRef: {fieldPath: metadata.name}}}
            # bindings and credentials only (Secret magpie, section 12)
            - {name: MAGPIE_DATABASE_URL,         valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_DATABASE_URL}}}
            - {name: MAGPIE_S3_ENDPOINT,          valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_S3_ENDPOINT}}}
            - {name: MAGPIE_S3_REGION,            valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_S3_REGION}}}
            - {name: MAGPIE_S3_BUCKET,            valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_S3_BUCKET}}}
            - {name: MAGPIE_S3_ACCESS_KEY_ID,     valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_S3_ACCESS_KEY_ID}}}
            - {name: MAGPIE_S3_SECRET_ACCESS_KEY, valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_S3_SECRET_ACCESS_KEY}}}
            - {name: MAGPIE_ACCEPT_PIGEON,        valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_ACCEPT_PIGEON}}}
            - {name: MAGPIE_ACCEPT_GATEWAY,       valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_ACCEPT_GATEWAY}}}
            - {name: MAGPIE_ACCEPT_HERMIT,        valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_ACCEPT_HERMIT}}}
            - {name: MAGPIE_ACCEPT_ADMIN,         valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_ACCEPT_ADMIN}}}
            - {name: MAGPIE_TOKEN_HERMIT,         valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_TOKEN_HERMIT}}}
            # the switch window's synchronous ingest (until A6): magpie calls http://magpie-import/v1/ingest-upload
            - {name: MAGPIE_TOKEN_IMPORT,         valueFrom: {secretKeyRef: {name: magpie, key: MAGPIE_TOKEN_IMPORT}}}
```

The bucket `yyc-ai-magpie` (Hetzner Object Storage, fsn1, project yyc, private, no versioning; created once with the
project's existing credential, inside the account's included storage) is the only new paid-for resource, and its cost
is zero at today's size (magpie spec 18.5: about 150 MB).

### 9.6 `hermit/`

hermit spec 21.1 with the platform values: namespace `yyc-ai`; ports 8080/9090 (Service `hermit` 80 → 8080); env
renamed per 3.7 (`HERMIT_ACCEPT_*`, `HERMIT_TOKEN_MAGPIE`, `HERMIT_TOKEN_EGRESS`; no `HERMIT_DB_CA`); node affinity to
the JuiceFS CSI nodes (`node.kubernetes.io/instance-type In [cx33, cpx32]`, as `ui` has today) because of the
`hermit-jfs` PVC; volumes `hermit-config` (hashed) at `/etc/hermit/config`, `catalogue` (unhashed, no `subPath`, so
updates land in place) at `/etc/hermit/catalogue`, PVC `hermit-jfs` at `/jfs` with `mountPropagation: HostToContainer`
(a restarted JuiceFS mount pod's new mount reaches the running hermit Pod, hermit 15.6), `/tmp` 256Mi.

`hermit/fabric.yaml`: the `hermit-gateway` DaemonSet (hermit spec 14.2) and `hermit-overlay-route` DaemonSet (hermit
spec 21.1) in namespace **`yyc-ai-fabric`** (PSS privileged, nothing else in it) instead of `kube-system`, so the
fenced Sealed Secrets applier needs a RoleBinding only in `yyc-ai` and `yyc-ai-fabric`, never in `kube-system`
(R24). Both DaemonSets use image `sandboxd` (9.2) and `busybox:1.37.0`.

`hermit/juicefs.yaml`: a second static PV for the existing JuiceFS volume:

```yaml
apiVersion: v1
kind: PersistentVolume
metadata: {name: hermit-jfs, labels: {juicefs-volume: yyc-ai-hermit}}
spec:
  capacity: {storage: 1Pi}
  volumeMode: Filesystem
  accessModes: [ReadWriteMany]
  persistentVolumeReclaimPolicy: Retain
  csi:
    driver: csi.juicefs.com
    volumeHandle: yyc-ai-hermit          # distinct handle → its own mount pod; same volume name in the Secret
    fsType: juicefs
    nodePublishSecretRef: {name: hermit-juicefs, namespace: yyc-ai}
  mountOptions: [cache-size=0, open-cache=0, all-squash=1000:1000]   # everything under ws/ is uid/gid 1000 (hermit 15.4)
---
apiVersion: v1
kind: PersistentVolumeClaim
metadata: {name: hermit-jfs, namespace: yyc-ai}
spec:
  accessModes: [ReadWriteMany]
  volumeMode: Filesystem
  resources: {requests: {storage: 1Pi}}
  storageClassName: ""
  selector: {matchLabels: {juicefs-volume: yyc-ai-hermit}}
```

### 9.7 `egress/` and `tools/`

Written in YYC from jarvis3's `deploy/chat/egress.yaml` and `tools.yaml` (which jarvis3 deletes, section 17), with:

- egress: `image: jarvis3-chat`, `args: [egress]` (17.2), ports `proxy` 3128, `api` 8082, `ops` 9090; env from
  Secret `egress` (section 12) plus `POD_IP`, `POD_NAMESPACE` fieldRefs; `egress-config` at `/etc/egress`; cache
  emptyDir 6Gi; resources 16.1; readiness `:9090/readyz`, preStop 10, grace 625 (3.11.6).
- Services (all `selector: {app: egress}`):

```yaml
apiVersion: v1
kind: Service
metadata: {name: egress, namespace: yyc-ai}       # hermit's fabric targets (EndpointSlices) and hermit's ops reads
spec:
  selector: {app: egress}
  ports:
    - {name: proxy, port: 3128, targetPort: proxy}
    - {name: http,  port: 80,   targetPort: api}
---
apiVersion: v1
kind: Service
metadata: {name: model, namespace: yyc-ai}        # the fixed endpoint name "model" for in-cluster callers (pigeon)
spec: {selector: {app: egress}, ports: [{name: http, port: 80, targetPort: api}]}
```

- tools: `args: [tools]`, port `http` 8080, `ops` 9090 (17.2), Service `tools` 80 → 8080, env from Secret `tools`,
  `tools-config` at `/etc/tools`, `catalogue` at `/etc/tools/catalogue` (its access checks), grace 180.

### 9.8 `cloudflared.yaml`

jarvis3's `deploy/chat/cloudflared.yaml` in namespace `yyc-ai`, same image `cloudflare/cloudflared:2026.10.0`, args
and probes, Secret `tunnel` key `token` = the new tunnel `yyc-ai-pigeon`'s token (14.1), resources 16.1.

### 9.9 `namespace.yaml`, `limits.yaml`

```yaml
apiVersion: v1
kind: Namespace
metadata:
  name: yyc-ai
  labels: {pod-security.kubernetes.io/enforce: restricted, pod-security.kubernetes.io/enforce-version: latest}
---
apiVersion: v1
kind: Namespace
metadata:
  name: yyc-ai-fabric
  labels: {pod-security.kubernetes.io/enforce: privileged}
```

LimitRange `defaults` in `yyc-ai`: default request 50m / 64Mi / ephemeral 64Mi, default limit 256Mi / ephemeral 1Gi,
max 4Gi / ephemeral 16Gi per container (jarvis3 `limits.yaml` today). PriorityClass `serving` exists cluster-wide
(news-stream base) and is used as is.

### 9.10 ConfigMaps: what is allowed

| ConfigMap | Read by | Holds | Why it is not a switch |
|---|---|---|---|
| `hermit-config` | hermit | `hermit.json`, prompt files | hermit's YYC implementation's own values (hermit spec 9) |
| `catalogue` | hermit, tools | who may use YYC AI, admins | hermit's YYC policy data |
| `egress-config` | egress | routes, model list, budget, allowlist options (17.2) | the YYC gateway's own values |
| `tools-config` | tools | tool providers and upstreams | the YYC tools' own values |
| `hermit-gateway` (yyc-ai-fabric) | the gateway installer | `host.json` | hermit's fabric |

Nothing is mounted into pigeon or magpie, and the sandbox gets only what hermit's descriptor hands the supervisor. The
check in 13.3 fails a commit that changes this.

### 9.11 RBAC

| Subject | Role | Rules |
|---|---|---|
| ServiceAccount `yyc-ai/hermit` | Role `hermit-read` (yyc-ai) | `apps/deployments` get, list; `pods` get, list; `discovery.k8s.io/endpointslices` get, list, watch |
| ServiceAccount `yyc-deploy/sealed-secrets-applier` (the fenced applier, new cluster) | RoleBinding to ClusterRole `sealed-secrets-applier` in `yyc-ai`, `yyc-ai-fabric` and `yyc-deploy`, in `clusters/talos/yyc-ai-access.yaml`; used by the deploy workflow by impersonation | SealedSecrets only |
| User `gha:yyc-ai` (GitHub OIDC, deploy-workflows 4.1, 12.3; new cluster only) | ClusterRole `yyc-ai-deployer-ns` bound in `yyc-ai`, `yyc-ai-fabric`; ClusterRole `yyc-ai-deployer-cluster` (PV `hermit-jfs`); Roles in `yyc` (the bridge placeholders `yyc-mcp`, `web` until T4; `yyc-catalogue` after yyc-mcp's move), `yyc-deploy` (impersonate `sealed-secrets-applier`; `inventory-yyc-ai`) | the kinds the render contains, `endpointslices` included; no `secrets` verb, no namespaces, no RBAC outside its namespaces (deploy-workflows 4.2) |
| ServiceAccount `jarvis/magpie-import` (old cluster, `yyc-ai/legacy/`, A1 to A6) | Role `magpie-import-legacy` in `jarvis` | `pods` get, list, watch; `pods/exec` create; `pods/proxy` get, create (bridge B7, 25.3); its token Secret is sealed into `yyc-ai/magpie-import` on the new cluster |
| every other Pod | none | `automountServiceAccountToken: false` |

### 9.12 The switch's importers (`jobs/`, present only during the switch; R15, cutover 7-8, DESIGN 9)

| Object | Image, command | Runs | Reads | Writes |
|---|---|---|---|---|
| Jobs `magpie-import-plan`, `magpie-import`, `magpie-import-catch-up-<n>` | `magpie`, `magpie import plan\|run\|catch-up` (+ `verify --all`) | A2 | `cloudcli`, `jarvis` (read-only transactions), PVC `hermit-jfs` at `/jfs` read-only | magpie's database and bucket |
| Deployment `magpie-import` (2 replicas, `RollingUpdate maxSurge 1 maxUnavailable 0`, PDB `minAvailable 1`, hostname spread) | `magpie`, container `magpie-import` args `[import, handover]`; sidecar `export-server` args `[import, export-server]` | from A3's `start-handover` until P4 ends (switch-final C14.3); the hand-over loop as the advisory-lock leader (`pg_try_advisory_lock(hashtext('magpie-import.leader'))` on the magpie database, held for the process's life), the running mirror, upload ingestion, warm hints; a Pod without the lock is ready and waits. One-shot commands by `kubectl -n yyc-ai exec deploy/magpie-import -- magpie import <cmd>` (cutover 7.9). `:9090/status`, `/readyz`, `/healthz`, `/metrics` (in no Service) | `cloudcli`, `jarvis` (read-only), PVC `hermit-jfs` read-only (the sidecar read-write), ui pods :8080 (`/__jarvis/sockets` of the router's switch module), the Kubernetes API (Pods in `jarvis`, exec) | magpie's database and bucket; the sidecar on a reverse hand-over: JuiceFS `users/`, `sent/`, database `cloudcli` |
| CronJob `hermit-import` (every minute from A2 to A6, `concurrencyPolicy: Forbid`, `activeDeadlineSeconds: 300`, `suspend: true` until A2) | `hermit`, `hermit import-jarvis [--delta]` | hermit 22.3 step 2 as amended by cutover 8.1 | `jarvis` (read-only), `/jfs` read-write (clones) | hermit's database, JuiceFS |
| CronJob `egress-import` (same schedule) | `jarvis3-chat`, args `[egress, import-jarvis]` | 17.2 item 6, cutover 8.2 (the new egress's budget counts the old stack's spend within a minute; the old egress does not see new-stack spend, cutover 8.3) | `jarvis` (read-only) | egress's database |
| CronJob `tools-import` (A2 once, A6 once: `suspend` toggled in git) | `jarvis3-chat`, args `[tools, import-jarvis]` | 17.2 tools item 4 | `jarvis` (read-only) | tools' database |

Every importer is idempotent by source id and restartable. Labels `app: import` plus `import: <name>` (the
NetworkPolicies of 10.2 select `app: import`). Secrets: the service's own Secret (`magpie`, `hermit`, `egress`,
`tools`) for its database and bucket, plus the importer's Secret (`magpie-import`, `hermit-import`, `egress-import`,
`tools-import`, 12.2) for the sources. ServiceAccount `magpie-import` with Role `magpie-import-legacy`
(Pods get, list, watch; `pods/exec` create) bound in namespace `jarvis` (both in `legacy/`, deleted with it). Resources: `magpie-import` 200m /
1 CPU, 1Gi (memory request = limit); the CronJobs 100m / 500m, 256Mi. No `magpie-import` image, crate binary or
workflow exists (R15, R17). Everything here is deleted in the teardown commit (18.4), and the subcommands with it.

---

## 10. NetworkPolicies

### 10.1 Namespace-wide (`network.yaml`)

```yaml
# nothing in yyc-ai accepts or opens a connection unless a policy below says so
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: default-deny, namespace: yyc-ai}
spec: {podSelector: {}, policyTypes: [Ingress, Egress]}
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: dns, namespace: yyc-ai}
spec:
  podSelector: {}
  policyTypes: [Egress]
  egress:
    - to: [{namespaceSelector: {matchLabels: {kubernetes.io/metadata.name: kube-system}},
            podSelector: {matchLabels: {k8s-app: kube-dns}}}]
      ports: [{port: 53, protocol: UDP}, {port: 53, protocol: TCP}]
---
# Postgres: db/pg for hermit, egress, tools (and the importers, which read jarvis and cloudcli there);
# db/magpie-pg for magpie and the importers (R31). Each database is closed to every other role (11.1)
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: postgres, namespace: yyc-ai}
spec:
  podSelector: {matchExpressions: [{key: app, operator: In, values: [hermit, egress, tools, import]}]}
  policyTypes: [Egress]
  egress:
    - to: [{namespaceSelector: {matchLabels: {kubernetes.io/metadata.name: db}},
            podSelector: {matchLabels: {cnpg.io/cluster: pg}}}]
      ports: [{port: 5432, protocol: TCP}]
---
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata: {name: postgres-magpie, namespace: yyc-ai}
spec:
  podSelector: {matchExpressions: [{key: app, operator: In, values: [magpie, import]}]}
  policyTypes: [Egress]
  egress:
    - to: [{namespaceSelector: {matchLabels: {kubernetes.io/metadata.name: db}},
            podSelector: {matchLabels: {cnpg.io/cluster: magpie-pg}}}]
      ports: [{port: 5432, protocol: TCP}]
```

`magpie-pg` is decided (decision 2), so `postgres-magpie` is always written.

Kubelet probes reach 9090 from the node without a rule (on the new cluster Cilium admits the local node's own
traffic to its pods by default, `allow-localhost`; on the old one k3s's policy controller does, as the probes of
today's `jarvis` pods under `default-deny-ingress` show; N3's test includes a probe under default deny). The internet rule used below is
`{ipBlock: {cidr: 0.0.0.0/0, except: [10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 100.64.0.0/10, 127.0.0.0/8,
169.254.0.0/16]}}` plus `{ipBlock: {cidr: "::/0", except: ["fc00::/7", "fe80::/10", "::1/128"]}}`, written `INTERNET`.

### 10.2 Per component (pod ports; NetworkPolicies match after Service translation)

| Policy | Pods | Direction | Peer | Ports |
|---|---|---|---|---|
| `pigeon-in` | pigeon | ingress | cloudflared; A1 to A6, `pigeon-from-bridge`: `10.0.255.2/32` (bridge `yyc-bridge-new`, carrying the old router's pass-on of old-path HTTP after the move, DESIGN 9.3, bridge B2) | 8080 |
| `pigeon-out` | pigeon | egress | magpie 8080, hermit 8080, egress 8082, INTERNET 443 (Access JWKS and get-identity) | |
| `magpie-in` | magpie | ingress | pigeon, hermit, egress, importers `app=import` | 8080 |
| `magpie-out` | magpie | egress | hermit 8080, INTERNET 443 (the bucket, `fsn1.your-objectstorage.com`); A3 to A6: Pods `app=import, import=magpie-import` 8080 (Service `magpie-import` 80 → the `export-server` sidecar's 8080, the synchronous ingest; WP-51) | |
| `hermit-in` | hermit | ingress | pigeon, magpie, egress, tools, importers `app=import`; while the monitor runs on the old cluster, `10.0.255.2/32` (bridge B4); after its move, `yyc/monitor` (namespaceSelector `yyc` + podSelector `app=monitor`) | 8080 |
| `hermit-out` | hermit | egress | magpie 8080, egress 8082, the API server (6443, as a `CiliumNetworkPolicy` `hermit-out-api` with `toEntities: [kube-apiserver]`: under Cilium an `ipBlock` of a control node's address does not match, DESIGN 15.2 N3; it carries hermit's EndpointSlice watch), fabric `10.202.0.0/24:7443`, sandboxes `10.203.0.0/16:7070`, INTERNET 443 (EC2 API) | |
| `egress-in` | egress | ingress | sandboxes `10.203.0.0/16` on 3128 only (R32); pigeon, hermit on 8082 | |
| `egress-out` | egress | egress | magpie 8080, hermit 8080, tools 8080, INTERNET all ports (the allowlist in egress decides what a sandbox reaches; OpenRouter for `model`) | |
| `tools-in` | tools | ingress | egress | 8080 |
| `tools-out` | tools | egress | hermit 8080; until T4 `10.0.255.1/32` on 8081 and 8082 (yyc-mcp and capture through `yyc-bridge-old`, B5-B6), after it `yyc/yyc-mcp` 8080 and `yyc/web` 8080; INTERNET 443 (Exa, TikHub, AnyAPI) | |
| `cloudflared-out` | cloudflared | egress | pigeon 8080, INTERNET (tcp/udp 7844 and tcp 443: Cloudflare's edge) | |
| `import-out` | importers `app=import` (9.12) | egress | magpie 8080, hermit 8080, the OLD cluster's API `10.0.0.3/32:6443` (`magpie-import`: ui Pods in `jarvis`, exec and pod proxy, bridge B7), `10.0.255.1/32:8080` (LU reads, B1), INTERNET 443 (bucket) | |
| `pigeon-to-ui` (A1 to A6) | pigeon | egress | `10.0.255.1/32:8080` (the legacy upstream through `yyc-bridge-old`, B1) | |
| `postgres-shared` (namespace-wide) | every pod of `yyc-ai` that has a `pg` database or reads one (hermit, egress, tools, importers) | egress | `10.0.0.2/32:30432` (`db/pg-shared`'s endpoint, the old cluster's `pg-hosts`, B3; after T5 the new `pg` pods) | |

Outside `yyc-ai`, in the same commit (on the old cluster unless stated; 25.3 for the bridges):
- `db/pg-ingress` (old cluster, news-stream/deploy/db/postgres.yaml): gains 5432 from `10.0.0.0/16` (the new
  cluster's clients through `pg-hosts`, arriving from their node's address, and the new fabric's hosts, arriving from
  the gateway node's address; bridge B3). Its live rules stay (read 2026-11-03): 5432 from `10.244.0.0/16` (the old
  stack's pods and the old fabric's JuiceFS mount pods, until A6) and from the old fabric's `10.201.0.0/24` (removed at
  teardown), and all ports from `10.244.0.0/16` (the operator and replication). The A1 commit's diff of the policy adds
  one `ipBlock` and removes nothing (cutover 10.1 step 1 checks it). No `magpie-pg` exclusion: `magpie-pg` is on the
  new cluster.
- `yyc/yyc-mcp`'s and `yyc/web`'s ingress policies admit `10.0.255.1/32` (bridge `yyc-bridge-old`, B5-B6)
  beside `jarvis` tools pods (both stay until teardown, or until those apps move in T4).
- The monitor's egress (if `yyc` restricts it): `10.0.255.2/32:8083` (B4) until it moves.
- `jarvis/ui-from-bridge` (in `yyc-ai/legacy/`, A1 to A6): ui 8080 from `10.0.255.1/32` (B1; deleted with `legacy/`).
- `jarvis/ui-to-pigeon` (in `yyc-ai/legacy/`, A1 to A6): ui egress to `10.0.255.2/32:8080` (the switch module's HTTP
  pass-on through `yyc-bridge-new`, B2; deleted with `legacy/`).
- `db/magpie-pg-ingress` (new cluster, with the Cluster, pg-ha 4.1): 5432 and the instance status port 8000 by
  selector only: `podSelector cnpg.io/cluster: magpie-pg` (the instances themselves, so the standbys' streaming and
  the instances' own status calls pass), the CloudNativePG operator's pods (`namespaceSelector` on the operator's
  namespace, the builder's, plus its pod label), and `namespaceSelector kubernetes.io/metadata.name: yyc-ai` with
  `podSelector` `app=magpie` or `app=import`, and the dump CronJob's pods `app=pg-dump-magpie` in `db`. Never an address: under Cilium an `ipBlock` does not match pod
  addresses, so `<pod-cidr>` would admit nothing (DESIGN 15.2 N3); never the sandbox hosts. The database itself admits
  only role `magpie`, and `yyc-ai`'s `postgres-magpie` lets only magpie and the importers out to it.

Harness parity: section 7.7 applies the same table, so a missing rule fails E19/E9 in CI first.

---

## 11. Databases (Clusters `magpie-pg` and `pg`)

### 11.1 New databases and roles (`news-stream/deploy/db/postgres.yaml`, Kustomization `yyc-db` on the old cluster, prune false; `magpie-pg` in the new cluster's `clusters/talos/db/`, 25.4)

`magpie` lives in its own Cluster `magpie-pg` (R31, decision 2; the manifest is magpie 18.2's plus pg-ha 4, in
`news-stream/deploy/db/magpie-pg.yaml` under the same Kustomization `yyc-db`, bootstrapped with database `magpie`
owned by role `magpie` from `pg-role-magpie`).
The other three are added to Cluster `pg`:

```yaml
# in Cluster pg spec.managed.roles:
  - {name: hermit, login: true, ensure: present, passwordSecret: {name: pg-role-hermit}}
  - {name: egress, login: true, ensure: present, passwordSecret: {name: pg-role-egress}}
  - {name: tools,  login: true, ensure: present, passwordSecret: {name: pg-role-tools}}
---
apiVersion: postgresql.cnpg.io/v1
kind: Database
metadata: {name: hermit, namespace: db}
spec: {cluster: {name: pg}, name: hermit, owner: hermit}
# … the same for egress, tools
```

Each database: `REVOKE CONNECT ON DATABASE <db> FROM PUBLIC` (a one-time statement as `postgres`, recorded in the
db README beside today's similar ones), so only its owner connects; role settings `ALTER ROLE <r> SET
statement_timeout = '15s'`, `idle_in_transaction_session_timeout = '60s'` (the services set their own per
connection too). Application names `<service>:<pod>`.

| Database | Owner | Contents | Expected size (2026-11-02 data) |
|---|---|---|---|
| `magpie` (Cluster `magpie-pg`) | magpie | magpie spec 4 | < 100 MB |
| `hermit` | hermit | hermit spec 10.2 | < 20 MB |
| `egress` | egress | `egress_log` (+ `person`, `request` columns), `budget_state` (17.2); the retired `allowed_hosts` and `confirmations` are not recreated (A13.F1) | 2 MB (4,783 log rows today) |
| `tools` | tools | `tool_calls` | < 1 MB (475 rows today) |
| `jfs_yyc_ai` | juicefs | unchanged (JuiceFS metadata; `/workspace` only after the cleanup) | 9 MB |
| `jarvis`, `cloudcli` | jarvis, cloudcli | dropped in A6 after the final dumps and the old chat UI's archive (decision 5, 11.3) | 17 MB, 19 MB |

### 11.2 Backups

`pg-dump-daily.yaml`: the list becomes `app:app:APP jfs_yyc_ai:juicefs:JUICEFS mcp:yyc_mcp:MCP hermit:hermit:HERMIT
egress:egress:EGRESS tools:tools:TOOLS` (plus `jarvis` and `cloudcli` until they are dropped) against `pg-rw`, and
`magpie:magpie:MAGPIE` against `magpie-pg-rw` (the dump script gains an optional host per entry), with `PW_<VAR>` from
`pg-role-<name>`. The 03:00 base backup and PITR (3 days) cover every database of `pg` already; `magpie-pg` archives
WAL and takes its nightly base backup (ScheduledBackup `magpie-pg-nightly`, 03:15 HKT, `target: prefer-standby`) to
the same ObjectStore `pg-pitr` under server name `magpie-pg` (magpie 18.2, pg-ha 4); `pg-nightly` (03:00 HKT) is unchanged (the single
`pg` instance). The old chat UI's archive (decision 5) goes to
`yyc-pg-backups/archive/`, outside the dumps' expiry rule (cutover 15.4).

### 11.3 Dropping `jarvis` and `cloudcli`

In A6 (cutover 10.5 step 6), after the final locked dump and the old chat UI's archive (decision 5): remove them from
the dump list, delete the `Database` CRs (`databaseReclaimPolicy` is `retain` by default in CNPG, so the database stays)
and run `DROP DATABASE jarvis; DROP DATABASE cloudcli; DROP ROLE jarvis; DROP ROLE cloudcli;` as `postgres`,
recorded in the task record; remove `pg-role-jarvis`, `pg-role-cloudcli` from `secrets.txt` and the managed roles
(`ensure: absent` first, then removed).

### 11.4 Connection budget

`pg`: `max_connections` 300; used today about 106 (app 45, jarvis 24, yyc_edge 20, others). `magpie-pg`:
`max_connections` 150 (magpie 18.2). At the largest scale (HPA maxima plus one surge Pod during a rollout):

| Service | Cluster | Per Pod | Pods (max + surge) | Total |
|---|---|---|---|---|
| magpie | `magpie-pg` | pool 12 + LISTEN 1 + leader 1 = 14 | 7 | 98 |
| `magpie-import` (switch only) | `magpie-pg` 4, `pg` 2 (the read-only sources) | 6 | 2 | 8 + 4 |
| hermit | `pg` | pool 10 + leader 1 + listener 1 = 12 | 3 | 36 |
| egress | `pg` | 8 | 3 | 24 |
| tools | `pg` | 4 | 3 | 12 |
| import CronJobs (switch only) | `pg` | 4 each | 3 | 12 |

`magpie-pg`: 106 < 150. `pg`: 88 + about 82 for everything else (today's 106 minus `jarvis`'s 24) = 170 < 300. At the normal 2 replicas each the new stack holds 28
connections on `magpie-pg` and 48 on `pg`. magpie's pool of 12 replaces magpie spec 17's 20 (R11); 12 connections per
replica serve 10 concurrent staff with a wide margin (each request holds a connection for milliseconds; long-polls
hold none, they wait on the LISTEN watch).

### 11.5 Cluster `pg` stays single-instance (decision 10; a stated gap)

Cluster `pg` keeps its one instance `pg-1` on `yyc-pool-db-worker1` (2 CPU / 6 Gi, 100 Gi, `primaryUpdateMethod:
restart`), as it runs today. Decision 8 asked for high availability; `pg-ha.md` designed it (two standbys on a new pool
of two cpx32, EUR 98.904 a month) and decision 10 declined it. `pg-hosts` (NodePort 30432) stays
`externalTrafficPolicy: Local`, so the sandbox hosts dial `10.0.0.2:30432` (hermit's `YYC_HERMIT_JUICEFS_META_URL` as
the old fabric's `YYC_JUICEFS_HOST_META_URL`). magpie, whose write path must not pause, is not on it (`magpie-pg`,
R31).

What a `pg` restart (operator or minor upgrade, a reboot or k3s upgrade of `db-worker1`) does to YYC AI, for its
duration (tens of seconds; a lost node is a restore from `pg-pitr`):

| Client | Effect | Handling |
|---|---|---|
| hermit | no new sandbox starts, no workspace calls | answers `unavailable` (503 + `retry-after`) and serves token keys, liveness, people and deployment facts from memory, so running turns go on; magpie's driver retries admissions until its 15 min deadline |
| egress | budget and log writes fail | serves its last budget status and buffers log rows (17.2 item 8); model calls continue |
| tools | `tool_calls` inserts fail | the row is dropped (A6.F30); calls continue |
| JuiceFS metadata (`jfs_yyc_ai`) | `/workspace` I/O on every host blocks | it retries its metadata transactions and resumes; `~/.claude` (magpie) is unaffected |

Readiness never depends on it (3.2), so no Service flaps. Planned restarts are done outside working hours and never
between A1 and A5 of the switch (DESIGN 7.3 item 2).

---

## 12. Secrets (Sealed Secrets, `infra/secrets.txt`, store `yyc`)

### 12.1 `seal.py` change

A source may join several store keys into one comma-separated value: `KEY=A|B?` gives `"<A>,<B>"`, where a trailing
`?` makes that part optional (skipped when the env lacks it). This is how a callee accepts the current and the next
pair token during a rotation (3.7) with no hand-edited value. `--check` treats optional parts as optional.

### 12.2 New lines in `infra/secrets.txt`

```
yyc-ai/pigeon        Opaque  yyc/deploy/sealed/pigeon.yaml        PIGEON_TOKEN_UI=YYC_TOKEN_PIGEON_UI PIGEON_LEGACY_VIEW_AS_KEY=YYC_PIGEON_LEGACY_VIEW_AS_KEY PIGEON_TOKEN_MAGPIE=YYC_TOKEN_PIGEON_MAGPIE PIGEON_TOKEN_HERMIT=YYC_TOKEN_PIGEON_HERMIT PIGEON_TOKEN_MODEL=YYC_TOKEN_PIGEON_MODEL PIGEON_VIEW_AS_KEYS=YYC_PIGEON_VIEW_AS_KEY|YYC_PIGEON_VIEW_AS_KEY_PREV? PIGEON_BLOB_KEYS=YYC_PIGEON_BLOB_KEY|YYC_PIGEON_BLOB_KEY_PREV?
yyc-ai/magpie        Opaque  yyc/deploy/sealed/magpie.yaml        MAGPIE_DATABASE_URL=YYC_MAGPIE_DATABASE_URL MAGPIE_S3_ENDPOINT=YYC_S3_ENDPOINT MAGPIE_S3_REGION='fsn1' MAGPIE_S3_BUCKET='yyc-ai-magpie' MAGPIE_S3_ACCESS_KEY_ID=YYC_S3_ACCESS_KEY MAGPIE_S3_SECRET_ACCESS_KEY=YYC_S3_SECRET_KEY MAGPIE_ACCEPT_PIGEON=YYC_TOKEN_PIGEON_MAGPIE|YYC_TOKEN_PIGEON_MAGPIE_NEXT? MAGPIE_ACCEPT_GATEWAY=YYC_TOKEN_GATEWAY_MAGPIE|YYC_TOKEN_GATEWAY_MAGPIE_NEXT? MAGPIE_ACCEPT_HERMIT=YYC_TOKEN_HERMIT_MAGPIE|YYC_TOKEN_HERMIT_MAGPIE_NEXT? MAGPIE_ACCEPT_ADMIN=YYC_TOKEN_ADMIN_MAGPIE|YYC_TOKEN_ADMIN_MAGPIE_NEXT? MAGPIE_TOKEN_HERMIT=YYC_TOKEN_MAGPIE_HERMIT MAGPIE_TOKEN_IMPORT=YYC_TOKEN_MAGPIE_IMPORT
yyc-ai/hermit        Opaque  yyc/deploy/sealed/hermit.yaml        HERMIT_DATABASE_URL=YYC_HERMIT_DATABASE_URL HERMIT_TOKEN_KEYS=YYC_HERMIT_TOKEN_KEYS HERMIT_ACCEPT_PIGEON=YYC_TOKEN_PIGEON_HERMIT|YYC_TOKEN_PIGEON_HERMIT_NEXT? HERMIT_ACCEPT_MAGPIE=YYC_TOKEN_MAGPIE_HERMIT|YYC_TOKEN_MAGPIE_HERMIT_NEXT? HERMIT_ACCEPT_GATEWAY=YYC_TOKEN_GATEWAY_HERMIT|YYC_TOKEN_GATEWAY_HERMIT_NEXT? HERMIT_ACCEPT_TOOLS=YYC_TOKEN_TOOLS_HERMIT|YYC_TOKEN_TOOLS_HERMIT_NEXT? HERMIT_ACCEPT_MONITOR=YYC_TOKEN_MONITOR_HERMIT|YYC_TOKEN_MONITOR_HERMIT_NEXT? HERMIT_ACCEPT_ADMIN=YYC_TOKEN_ADMIN_HERMIT|YYC_TOKEN_ADMIN_HERMIT_NEXT? HERMIT_TOKEN_MAGPIE=YYC_TOKEN_HERMIT_MAGPIE HERMIT_TOKEN_EGRESS=YYC_TOKEN_HERMIT_EGRESS HERMIT_FABRIC_CA=YYC_HERMIT_FABRIC_CA HERMIT_FABRIC_CLIENT_CERT=YYC_HERMIT_FABRIC_CLIENT_CERT HERMIT_FABRIC_CLIENT_KEY=YYC_HERMIT_FABRIC_CLIENT_KEY HERMIT_FABRIC_HOST_CERT=YYC_HERMIT_FABRIC_HOST_CERT HERMIT_FABRIC_HOST_KEY=YYC_HERMIT_FABRIC_HOST_KEY HERMIT_AWS_ACCESS_KEY_ID=YYC_HERMIT_AWS_ACCESS_KEY_ID HERMIT_AWS_SECRET_ACCESS_KEY=YYC_HERMIT_AWS_SECRET_ACCESS_KEY HERMIT_JUICEFS_META_URL=YYC_HERMIT_JUICEFS_META_URL HERMIT_JUICEFS_META_PASSWORD=YYC_PG_JUICEFS_PASSWORD
yyc-ai/hermit-juicefs Opaque yyc/deploy/sealed/hermit-juicefs.yaml name=YYC_JUICEFS_VOLUME_NAME metaurl=YYC_JUICEFS_META_URL envs=YYC_JUICEFS_CSI_ENVS
yyc-ai/egress        Opaque  yyc/deploy/sealed/egress.yaml        EGRESS_DATABASE_URL=YYC_EGRESS_DATABASE_URL OPENROUTER_API_KEY=YYC_AI_OPENROUTER_KEY EGRESS_ACCEPT_PIGEON=YYC_TOKEN_PIGEON_MODEL|YYC_TOKEN_PIGEON_MODEL_NEXT? EGRESS_ACCEPT_HERMIT=YYC_TOKEN_HERMIT_EGRESS|YYC_TOKEN_HERMIT_EGRESS_NEXT? EGRESS_ACCEPT_ADMIN=YYC_TOKEN_ADMIN_EGRESS|YYC_TOKEN_ADMIN_EGRESS_NEXT? EGRESS_TOKEN_MAGPIE=YYC_TOKEN_GATEWAY_MAGPIE EGRESS_TOKEN_HERMIT=YYC_TOKEN_GATEWAY_HERMIT EGRESS_TOKEN_TOOLS=YYC_TOKEN_GATEWAY_TOOLS
yyc-ai/tools         Opaque  yyc/deploy/sealed/tools.yaml         TOOLS_DATABASE_URL=YYC_TOOLS_DATABASE_URL EXA_API_KEY=YYC_EXA_API TIKHUB_API=YYC_TIKHUB_API YYC_MCP_TOKEN=YYC_JARVIS_MCP_TOKEN WEB_FETCH_TOKEN=YYC_WEB_FETCH_TOKEN ANYAPI_API_KEY=YYC_ANYAPI_API_KEY TOOLS_ACCEPT_GATEWAY=YYC_TOKEN_GATEWAY_TOOLS|YYC_TOKEN_GATEWAY_TOOLS_NEXT? TOOLS_TOKEN_HERMIT=YYC_TOKEN_TOOLS_HERMIT
yyc-ai/tunnel        Opaque  yyc/deploy/sealed/tunnel.yaml        token=YYC_CF_PIGEON_TUNNEL_TOKEN
yyc-ai/magpie-import Opaque  yyc/deploy/sealed/magpie-import.yaml MAGPIE_IMPORT_CLOUDCLI_URL=YYC_CLOUDCLI_DATABASE_URL MAGPIE_IMPORT_JARVIS_URL=YYC_JARVIS_DATABASE_URL MAGPIE_IMPORT_TOKEN_UI=YYC_TOKEN_MAGPIE_IMPORT_UI MAGPIE_IMPORT_ACCEPT_MAGPIE=YYC_TOKEN_MAGPIE_IMPORT|YYC_TOKEN_MAGPIE_IMPORT_NEXT?
yyc-deploy/tunnel    Opaque  news-stream/deploy/apps/sealed/yyc-deploy-tunnel.yaml  token=YYC_CF_DEPLOY_TUNNEL_TOKEN
yyc-ai/hermit-import Opaque  yyc/deploy/sealed/hermit-import.yaml HERMIT_IMPORT_JARVIS_URL=YYC_JARVIS_DATABASE_URL
yyc-ai/egress-import Opaque  yyc/deploy/sealed/egress-import.yaml EGRESS_IMPORT_JARVIS_URL=YYC_JARVIS_DATABASE_URL
yyc-ai/tools-import  Opaque  yyc/deploy/sealed/tools-import.yaml  TOOLS_IMPORT_JARVIS_URL=YYC_JARVIS_DATABASE_URL
yyc-ai-fabric/hermit-gateway Opaque yyc/deploy/sealed/hermit-gateway.yaml host.crt=YYC_HERMIT_FABRIC_GATEWAY_CERT host.key=YYC_HERMIT_FABRIC_GATEWAY_KEY ca.crt=YYC_HERMIT_FABRIC_CA wg.key=YYC_HERMIT_GATEWAY_WG_KEY
db/pg-role-magpie    kubernetes.io/basic-auth  news-stream/deploy/apps/sealed/pg-role-magpie.yaml  username='magpie' password=YYC_PG_MAGPIE_PASSWORD
db/pg-role-hermit    kubernetes.io/basic-auth  news-stream/deploy/apps/sealed/pg-role-hermit.yaml  username='hermit' password=YYC_PG_HERMIT_PASSWORD
db/pg-role-egress    kubernetes.io/basic-auth  news-stream/deploy/apps/sealed/pg-role-egress.yaml  username='egress' password=YYC_PG_EGRESS_PASSWORD
db/pg-role-tools     kubernetes.io/basic-auth  news-stream/deploy/apps/sealed/pg-role-tools.yaml   username='tools' password=YYC_PG_TOOLS_PASSWORD
```

Changed: `yyc/monitor-keys` replaces `JARVIS_OPS_TOKEN=YYC_JARVIS_CC_TOKEN` with `HERMIT_OPS_TOKEN=YYC_TOKEN_MONITOR_HERMIT`
(in the switch commit, with monitor.py's change, 15.5).

pigeon's view-as and blob keys rotate the other way round (the signer is pigeon itself): the list's first key signs,
all verify; the rotation puts the new key first and keeps the old one as `_PREV` for 12 h (pigeon spec 3.3).

### 12.3 New store keys (`scripts/store-put.py yyc KEY=file:PATH --explain KEY=…`, no approval needed)

| Key | Value | Explanation to record |
|---|---|---|
| `YYC_TOKEN_<CALLER>_<CALLEE>` (the 14 pairs of 3.7) | 32 random bytes, base64url | "YYC AI pair token: <caller> calling <callee>; sealed into <caller>'s and <callee>'s Secrets in namespace yyc-ai (DE0CH/YYC infra/secrets.txt)" |
| `YYC_PIGEON_VIEW_AS_KEY`, `YYC_PIGEON_BLOB_KEY` | 32 random bytes, base64url | pigeon's HMAC keys (pigeon spec 3.3) |
| `YYC_HERMIT_TOKEN_KEYS` | `1:<base64url 32 bytes>` | hermit spec 6.1 |
| `YYC_PG_{MAGPIE,HERMIT,EGRESS,TOOLS}_PASSWORD` | 32 random bytes, hex | role passwords, "embedded in YYC_<NAME>_DATABASE_URL (change both)" |
| `YYC_{HERMIT,EGRESS,TOOLS}_DATABASE_URL` | `postgresql://<role>:<pw>@pg-shared.db.svc.cluster.local:5432/<db>?sslmode=require` | Cluster `pg` through `db/pg-shared` (B3; after T5 the new primary, DESIGN 15.8). Never `pg-rw.db`: on the new cluster that name does not resolve before T5 and names the T5 replica cluster's own Service after it |
| `YYC_MAGPIE_DATABASE_URL` | `postgresql://magpie:<pw>@magpie-pg-rw.db.svc.cluster.local:5432/magpie?sslmode=require` (decision 2) | |
| `YYC_MAGPIE_IMPORT_UI_PEER` | hex of `sha256("jarvis-ui-peer\|" + YYC_JARVIS_CC_TOKEN)` (cutover 7.2) | the importer's key for ui's peer-holds route; deleted at teardown |
| `YYC_HERMIT_JUICEFS_META_URL` | today's `YYC_JUICEFS_HOST_META_URL` value (the NodePort path, now reached through the new gateway) | hermit spec 8.4 |
| `YYC_HERMIT_FABRIC_*`, `YYC_HERMIT_GATEWAY_WG_KEY`, `YYC_HERMIT_AWS_*` | from `fabric/host/make-certs.sh` and `fabric/aws/infra.sh iam-key` | hermit spec 14.4-14.5 |
| `YYC_CF_PIGEON_TUNNEL_TOKEN` | tofu output `tunnel_token_pigeon` | 14.1 |
| `YYC_TOKEN_PIGEON_UI`, `YYC_TOKEN_MAGPIE_IMPORT_UI` | 32 random bytes, base64url | switch window only (A1-A6): pigeon's legacy upstream marker and the importer's and runner's bearer at the old router's switch module (DESIGN 9.6); sealed into `pigeon`, `magpie-import` and, for the A1 ui rollout, `jarvis/ui-env` (`JARVIS_UI_LU_TOKEN`, `JARVIS_UI_SWITCH_TOKEN`) |
| `YYC_TOKEN_MAGPIE_IMPORT` | 32 random bytes, base64url | switch window only (P1-A6): magpie's bearer at the import Deployment's synchronous ingest (WP-51); sealed into `magpie` (`MAGPIE_TOKEN_IMPORT`, from P1, since magpie requires it at start) and `magpie-import` (`MAGPIE_IMPORT_ACCEPT_MAGPIE`) |
| `YYC_PIGEON_LEGACY_VIEW_AS_KEY` | `sha256("jarvis-view-as\|" + JARVIS_CC_TOKEN)`, derived once by an operator script | switch window only: pigeon verifies the old router's view-as tokens (switch-final 11) |
| `YYC_CF_DEPLOY_TUNNEL_TOKEN` | tofu output `yyc_deploy_tunnel_token` | connector token of tunnel `yyc-deploy` (the k8s API for YYC's deploy workflow); sealed into `yyc-deploy/tunnel` (deploy-workflows 7) |
| `YYC_DEPLOY_ACCESS_CLIENT_ID`, `YYC_DEPLOY_ACCESS_CLIENT_SECRET` | tofu outputs of the Access service token `yyc-deploy (GitHub Actions)` | open `k8s-deploy.yyc-internal.com`; GitHub environment `yyc-ai` secrets (12.4) |

All keys are written when made, with their explanation and `stores.txt` line (the standing rule). Deleted from the
store at teardown (18.4): `YYC_JARVIS_CC_TOKEN`, `YYC_JARVIS_SENT_FILES_TOKEN`, `YYC_CLOUDCLI_JWT_SECRET`,
`YYC_SBX_*`, `YYC_CF_AI_TUNNEL_TOKEN`, `YYC_SANDBOX_GHCR_DOCKERCONFIGJSON`, `YYC_AI_AMI_AWS_*`, the window keys above; after 11.3
`YYC_PG_JARVIS_PASSWORD`, `YYC_PG_CLOUDCLI_PASSWORD`, `YYC_JARVIS_DATABASE_URL`, `YYC_CLOUDCLI_DATABASE_URL`.

### 12.4 GitHub Actions secrets

| Repo | Secret | For |
|---|---|---|
| DE0CH/aviary | none (`GITHUB_TOKEN` pushes packages and releases) | |
| DE0CH/YYC | `YYC_HERMIT_AMI_AWS_ACCESS_KEY_ID`, `YYC_HERMIT_AMI_AWS_SECRET_ACCESS_KEY` (IAM user `hermit-ami-ci`) | the AMI workflow (13.5); replaces `YYC_AI_AMI_AWS_*` and `YYC_SANDBOX_GHCR_DOCKERCONFIGJSON` (images are public) |
| DE0CH/YYC, environment `yyc-ai` (deployment branch `main` only) | `YYC_DEPLOY_ACCESS_CLIENT_ID`, `YYC_DEPLOY_ACCESS_CLIENT_SECRET` | the `deploy` job's path to the API (deploy-workflows 7); no cluster credential (OIDC) |
| DE0CH/jarvis3 | none new | |

---

## 13. The deploy workflow, the YYC checks and the AMI workflow

### 13.1 Who applies what (decision 1; text of record `deploy-workflows.md`)

Two clusters (decision 13, 25): on the old cluster Kustomizations `yyc-ai` and `yyc-ai-secrets` keep applying the
old stack from `yyc-ai/legacy/` (Flux, until the Talos session removes that cluster; there is no hand-over). On the
new cluster `yyc/deploy` is applied by workflow `DE0CH/YYC/.github/workflows/yyc-ai.yml`
(deploy-workflows 5-6): job `check` on every PR and push touching YYC AI, job `deploy` on push to `main`, every 3 h
against drift and on `workflow_dispatch`. The runner reaches the API server through tunnel `yyc-deploy` (Access
service token, `cloudflared access tcp` to `127.0.0.1:6443`) and authenticates with a GitHub OIDC token minted per
kubectl call, which the API server's structured authentication config accepts only from DE0CH/YYC `main`, environment
`yyc-ai`, workflow `yyc-ai.yml` (user `gha:yyc-ai`, 9.11; the authenticator is in the new control nodes' Talos
machine config, the connector of tunnel `yyc-deploy` on the new cluster). The applier is `tools/yyc-apply` (Rust): render, plan
against the live state with server-side dry runs, the deletion gate (`Deploy-Prune:`, `Deploy-Prune-Namespace:`,
`Deploy-Release:`, `Deploy-Replace:` trailers), apply in order (cluster-scoped; Sealed Secrets impersonating
`yyc-deploy/sealed-secrets-applier`, then a wait for each changed SealedSecret's `Synced`; non-workloads;
workloads), `rollout status` of every workload, prune, inventory ConfigMap `yyc-deploy/inventory-yyc-ai`. The new
cluster's own workflow (the Talos session's) applies its cluster add-ons, the deploy path itself, YYC AI's namespaces
and RBAC, the JuiceFS CSI driver and its databases (`magpie-pg`, `pg-shared`). On the old cluster Flux keeps
`yyc-apps`, `yyc-apps-secrets`, `yyc-db` (prune false: 11.1, Cluster `pg` and the new roles) and
`yyc-aiwatch(-secrets)` besides `yyc-ai`/`yyc-ai-secrets`.

### 13.2 The deploy flow (state machine of one change)

```
aviary commit ──ci.yml──► gates pass ──publish──► images <sha12> + release aviary-<sha12>      (or: nothing published)
      │
      ▼  (Claude, in a session holding no extra store)
YYC commit: bump pigeon/magpie/hermit tags (same sha12); bump hermit.json sandbox.image only if the
supervisor or image changed; bump fabric.sandboxd only if sandboxd changed
      │
      ▼  yyc-ai.yml job `check` on the push (13.3); red = nothing applied
job `deploy` (minutes after the push): plan, deletion gate, apply in order, `rollout status` of every workload
      │
      ▼  Deployments roll one surge Pod at a time (maxUnavailable 0); the AMI workflow bakes in parallel if pinned
verify: ops page verdict `ok`; `yyc/tests/live_check.py` (15.6) green (the run already waited for every rollout)
      │
      ├─ green ─► done (task record: the pins, the run link)
      └─ red ───► revert the YYC commit (the previous pins; a revert that removes objects carries its `Deploy-Prune`
                  trailers, printed by `yyc-apply plan --trailers`); the schema only grew, so old images run on it
```

Order inside one YYC commit does not matter to the services: every component accepts its peers' previous protocol
(3.4); one run applies the whole set in the order of deploy-workflows 6.4. A protocol bump that drops an old number
ships in a later aviary release than the one that added the new number.

### 13.3 Job `check` of `yyc-ai.yml` (DE0CH/YYC; on pull_request and push touching `yyc/deploy/**`, `news-stream/deploy/db/**`, `infra/secrets.txt`, `tools/yyc-apply/**`, the workflow)

No cluster credentials; it reads the repo and public registries only. (The former `yyc-ai-check.yml` and
`yyc-ai-render.yml` are this job; the ZDR guard runs in it too, and keeps its own daily `yyc-ai-zdr.yml`.)

1. `kustomize build yyc/deploy` and `kustomize build news-stream/deploy/db`, then `kubeconform -strict` (v0.8.0)
   with schemas for SealedSecret, CNPG Cluster/Database, HelmRelease, HPA.
2. **No switches:** containers named `pigeon`, `magpie` have only `secretKeyRef`/`fieldRef` env, no `envFrom`, no
   ConfigMap volume, args exactly `[serve]`; every `yyc-ai` Deployment's env names match `^[A-Z0-9_]+$` and appear in
   `infra/secrets.txt` or are fieldRefs (no literal `value:` env at all in `yyc-ai`).
3. **Zero downtime shape:** every Deployment in `yyc-ai`: `maxUnavailable: 0`, `maxSurge: 1`, a readinessProbe, a
   preStop, `terminationGracePeriodSeconds` ≥ the table of 9.3, a PDB selecting it with `minAvailable: 1`, either
   `replicas ≥ 2` or an HPA with `minReplicas ≥ 2` (and then no `replicas` field), the hostname spread constraint.
   The switch-only Deployment `magpie-import` (label `app: import`, 9.12) meets the same shape (2 replicas, a PDB, a
   leader lock); the check names it explicitly because its container is not `serve`, and the name is deleted with it.
4. **Pins:** pigeon, magpie and hermit share one sha12; each pinned tag and the sandbox image digest exist in GHCR
   (anonymous `crane digest`; fails when a package is still private); the sandbox image's `dev.de0ch.aviary.speaks`
   numbers are in the pinned magpie's and hermit's `dev.de0ch.aviary.accepts`; `fabric.sandboxd`'s release exists and
   its sha12 equals the `sandboxd` image tag.
5. **Secrets:** `infra/seal.py --check --names-only` (the map, the sealed folders and their kustomizations agree;
   no env needed for this mode).
6. **Policies:** every NetworkPolicy podSelector `app` label matches a workload in the rendered output; every
   Service's selector matches a workload.
7. **Catalogue:** `catalogue.json` parses under hermit's rules (`hermit catalogue-check` from the pinned hermit image:
   a read-only subcommand that validates the file and exits).
8. **Gateway config agrees with the fabric:** `egress/egress.json` parses; its `routes` are `model`, `tools` (only
   `/v1/`) and configured routes (3.11.8), each with `allowPaths` and named by a managed skill; `selfHosts` contains `hermit/hermit.json`'s egress VIP (the one address both files name, 3.11.1); every
   `${VAR}` in `routes[].inject` is a key of Secret `egress` in `infra/secrets.txt`.
9. **Deployer rights:** every rendered (namespace, kind, name) is inside deploy-workflows 4.2's table (else "extend
   `clusters/hetzner/yyc-ai-access.yaml`" first).
10. **Sealed folder:** `sealed/` renders only `kind: SealedSecret`, each with a matching `infra/secrets.txt` line.
11. **Trailers:** `Deploy-*` trailers in the pushed commits are well-formed; `Deploy-Replace` is refused for
    Deployments, DaemonSets, StatefulSets, PDBs and Services in `yyc-ai` and for PVs/PVCs.
12. **No cluster-level objects:** the render contains no `Namespace` and no RBAC outside `yyc-ai`, `yyc-ai-fabric`,
    `jarvis`.
13. **Switch window** (while `yyc-ai/cutover/window.json` exists, A1 to A5): `cutover freeze-check` (cutover 11.1).

### 13.4 Image pins, drift and restarts

Server-side apply restores only the fields the deploy's field manager (`yyc-ai-deploy`) owns. A `kubectl rollout
restart` writes `kubectl.kubernetes.io/restartedAt` under manager `kubectl-rollout`, which the deploy does not undo, so
an operator restart now sticks; a roll that must survive a rebuild still goes through git: an image pin, a hashed
ConfigMap, or the per-Deployment `yyc/secrets-rev` annotation. The 3-hourly scheduled run restores any object deleted
or any owned field edited by hand.

### 13.5 `hermit-ami.yml` (DE0CH/YYC; replaces `yyc-ai-ami.yml`)

```yaml
name: hermit-ami
# Keeps hermit's AWS host AMI in step with what hermit.json pins: the sandbox image (sandbox.image), the sandboxd
# release (fabric.sandboxd) and the gVisor release (fabric.runsc). Bakes only when one of the three changed (an
# allowlist, prompt or catalogue edit never bakes), with the fabric scripts of the aviary commit YYC pins (not main).
on:
  push:
    branches: [main]
    paths: ["yyc/deploy/hermit/hermit.json", ".github/workflows/hermit-ami.yml"]
  workflow_dispatch:
    inputs: {force: {description: "bake even when the AMI already holds the pins", type: boolean, default: false}}
concurrency: {group: hermit-ami, cancel-in-progress: false}
jobs:
  ami:
    runs-on: ubuntu-24.04
    timeout-minutes: 60
    permissions: {contents: read}
    steps:
      - uses: actions/checkout@<sha>       # with fetch-depth 2 for the parent diff
        with: {fetch-depth: 2}
      - name: changed pins
        id: pins
        run: |
          set -euo pipefail
          f=yyc/deploy/hermit/hermit.json
          now=$(jq -c '[.sandbox.image, .fabric.sandboxd, .fabric.runsc]' "$f")
          before=$(git show HEAD^:"$f" 2>/dev/null | jq -c '[.sandbox.image, .fabric.sandboxd, .fabric.runsc]' || echo null)
          echo "changed=$([ "$now" != "$before" ] && echo 1 || echo 0)" >> "$GITHUB_OUTPUT"
          echo "aviary=$(yq '.images[] | select(.name=="hermit") | .newTag' yyc/deploy/kustomization.yaml)" >> "$GITHUB_OUTPUT"
      - name: aviary at the pinned commit
        if: steps.pins.outputs.changed == '1' || inputs.force
        run: |
          git clone --filter=blob:none https://github.com/DE0CH/aviary "$RUNNER_TEMP/aviary"
          git -C "$RUNNER_TEMP/aviary" checkout "${{ steps.pins.outputs.aviary }}"
      - name: crane
        if: steps.pins.outputs.changed == '1' || inputs.force
        run: curl -fsSL https://github.com/google/go-containerregistry/releases/download/v0.20.6/go-containerregistry_Linux_x86_64.tar.gz | sudo tar -xz -C /usr/local/bin crane
      - name: bake if stale, then check
        if: steps.pins.outputs.changed == '1' || inputs.force
        env:
          AWS_ACCESS_KEY_ID: ${{ secrets.YYC_HERMIT_AMI_AWS_ACCESS_KEY_ID }}
          AWS_SECRET_ACCESS_KEY: ${{ secrets.YYC_HERMIT_AMI_AWS_SECRET_ACCESS_KEY }}
          FORCE: ${{ inputs.force && '1' || '' }}
        run: |
          set -euo pipefail
          a="$RUNNER_TEMP/aviary/fabric/aws/infra.sh"
          bash "$a" --config yyc/deploy/hermit/hermit.json sync     # bakes when the template's AMI tags differ
          bash "$a" --config yyc/deploy/hermit/hermit.json check    # exit 0 = the template's AMI holds all three
```

`infra.sh` reads the three pins from `hermit.json` (no image argument), tags AMIs `image`, `sandboxd`, `runsc`, bakes
on a `t3.large` builder (about 15 min), points launch template `hermit-host` at it as its only version and prunes the
previous AMI and snapshot. Images are public, so no GHCR credential. Hosts launched while it bakes fetch the image from
the gateway (about 45 s more), as today.

### 13.6 The ZDR guard and the OpenRouter guardrail (DE0CH/YYC; A7.F51, A7.F52, A19.F21, A19.F26, A20.F15)

Both read `config.json` today, which the teardown deletes, and the guard finds the jarvis3 commit through the remote
base that R41 removes. Both are rewritten in the YYC commit that adds `yyc/deploy/egress/egress.json` (cutover P1),
so they pass against the new stack before the switch; while `legacy/` exists they check both stacks.

**`yyc/tests/test_zdr.py`** (stdlib only, as today), run by `yyc-ai-zdr.yml` (unchanged triggers: push to
`yyc-ai/**`, which covers `yyc/deploy/egress/**`, daily 03:17 UTC, dispatch; the workflow passes its
`GITHUB_TOKEN` as `GH_TOKEN` for the API call below). It fails when, for the new stack:
1. a model in `egress.json` `models[].id`, `defaultModel`, `smallModel` or `titleModel` has no endpoint in
   `GET https://openrouter.ai/api/v1/endpoints/zdr`;
2. `egress.json` `routes` has no route named `model` whose `upstream` contains `openrouter.ai` with `model: true`, or
   any route whose upstream contains `openrouter.ai` lacks `model: true`;
3. the jarvis3 commit cannot be found: the tag of image `jarvis3-chat` in `yyc/deploy/kustomization.yaml`'s
   `images:` (a `<sha12>`) is resolved to the full sha with `GET https://api.github.com/repos/DE0CH/jarvis3/commits/<sha12>`;
4. at that commit (raw.githubusercontent.com by full sha) the same three literals are missing: in
   `chat/src/egress/modelbody.ts` `export const FORCED_PROVIDER = { zdr: true, data_collection: "deny" }`; in
   `chat/src/egress/proxy.ts` `const isModel = route.model === true;`, `const rewrite = isModel && req.method !== "GET" && req.method !== "HEAD" ? new ModelBodyRewriter(`, `rewrite.push(chunk)`, `rewrite.end()` and `if (!rewrite) {` (and `...FORCED_PROVIDER,` closing the provider object in `modelbody.ts`); in
   `chat/test/egress.test.ts` the string `zero data retention is forced into every request body`. egress keeps these
   literals in these files (17.2 item 9); a jarvis3 change that moves them updates this guard in the YYC commit that
   pins it.
While `yyc/deploy/legacy/config.json` exists, the guard also runs today's three checks against it and the legacy
remote base's `?ref=` (the old stack serves until it drains); the teardown commit deletes that half.

**`yyc/infra/openrouter.py`**: `MODELS` = `egress.json` `models[].id` ∪ `defaultModel` ∪ `smallModel` ∪ `titleModel`,
plus, while `legacy/config.json` exists, its `models`, `defaultModel` and `titleModel` (so the draining old stack keeps
its models); the guardrail `yyc-chat`'s `allowed_models` follows on the next run. The key keeps the name `yyc-chat`, and
a rotation's temporary key `yyc-chat-next`, so the monitor's balances panel (every key whose name starts with `yyc-`,
A19.F26) keeps showing YYC AI's spend. Order in cutover P1: the YYC commit with `egress.json` and both scripts, then
`openrouter.py OUT` (updates the guardrail, so `smallModel` is allowed before any sandbox calls it), then
`openrouter.py OUT --verify`; both green are on the P1 checklist, before any staff traffic reaches the new egress.

---

## 14. Cloudflare (tunnel and the switch; `news-stream/deploy/cloudflare/tofu`, applied by `cloudflare.yml`)

### 14.1 The new tunnel (cutover P1; names fixed by R39)

```hcl
resource "cloudflare_zero_trust_tunnel_cloudflared" "pigeon" {
  account_id = local.account
  name       = "yyc-ai-pigeon"
  config_src = "cloudflare"
}
resource "cloudflare_zero_trust_tunnel_cloudflared_config" "pigeon" {
  account_id = local.account
  tunnel_id  = cloudflare_zero_trust_tunnel_cloudflared.pigeon.id
  config = {
    ingress = concat(flatten([for h in local.staff_ai_hosts : [
      { hostname = h, path = "^/(${join("|", local.staff_ai_paths)})(/.*)?$",
        service = "http://pigeon.yyc-ai.svc.cluster.local:80",
        origin_request = { access = { required = true, team_name = local.team,
                                      aud_tag = [cloudflare_zero_trust_access_application.staff_ai.aud] } } },
      { hostname = h, service = "http://pigeon.yyc-ai.svc.cluster.local:80" },
    ]]), [{ service = "http_status:404" }])
  }
}
data "cloudflare_zero_trust_tunnel_cloudflared_token" "pigeon" {
  account_id = local.account
  tunnel_id  = cloudflare_zero_trust_tunnel_cloudflared.pigeon.id
}
# outputs.tf, beside tunnel_token_stream, tunnel_token_mailmerge, tunnel_token_yyc_ai
output "tunnel_token_pigeon" { value = data.cloudflare_zero_trust_tunnel_cloudflared_token.pigeon.token, sensitive = true }
```

- One host list and one path list for both tunnels and the Access app: `local.staff_ai_hosts` gains the test host
  `pigeon-test.${local.domain}` (a proxied CNAME `pigeon_test` to the new tunnel; the Access app `staff_ai` gains its
  destinations through the same list, same audience), and `local.staff_ai_paths` stays today's
  `["api", "paseo-ws", "ws", "ops", "__jarvis"]` until the teardown. No second list (`pigeon_paths`) exists: Access on
  `ws` and `__jarvis` in front of pigeon is harmless (pigeon answers them 404), and pigeon's CI test (pigeon 17.5)
  checks that its signed-in prefixes are a subset of the list.
- The token goes into store `yyc` as `YYC_CF_PIGEON_TUNNEL_TOKEN` and is sealed into `yyc-ai/tunnel` (key `token`).

### 14.2 The switch commit (A3, switch-final 6; one tofu change in the same push as YYC commit B)

- `cloudflare_dns_record.origin_ai.content` → `"${cloudflare_zero_trust_tunnel_cloudflared.pigeon.id}.cfargotunnel.com"`
  (the record is proxied: the edge routes new requests to the new tunnel at once, no DNS TTL involved; the custom
  hostname `ai-yyc-ui.deyaochen.com` that uses `origin_ai` follows).
- `access.tf`'s catalogue read moves to `catalogue.json` (R40, hermit 21.5).
- The old tunnel `yyc_ai` and its config stay, and `staff_ai_paths` keeps `ws` and `__jarvis` until A6, so the
  WebSockets already open on the old connectors keep working until the runner's move closes them at a request-free
  instant, right after the settle time (DESIGN 9.3; rehearsal R0 proves the edge keeps them across the record
  change). The test host's record goes. The same YYC push pins pigeon's HPA (`minReplicas = maxReplicas`) until
  `30-ui-0` (DESIGN 9.3).

Rollback before teardown: revert this commit; the record points at the old tunnel again and `ui` serves (its stack
never changed).

### 14.3 Teardown (A6, with 18.4)

Delete `cloudflare_zero_trust_tunnel_cloudflared.yyc_ai`, its config and `tunnel_token_yyc_ai`; the test host's
place in `staff_ai_hosts` (its record went in A3); `ai-yyc-ui` from `saas.tf` if cutover 9.1 says so; `staff_ai_paths` becomes
`["api", "paseo-ws", "ops"]`. The store key `YYC_CF_AI_TUNNEL_TOKEN` and the `jarvis/tunnel` line go. aviary's
`pigeon/tests/signed_in_paths.rs` keeps comparing pigeon's signed-in prefixes with `access.tf` (pigeon 17.5).

---
## 15. Observability: logs, metrics, the ops page and the news-stream monitor

### 15.1 Logs

One JSON object per line on stdout (the supervisor writes to stderr, which is the sandbox console, supervisor spec
21.1), written by `aviary-core::log`:

```json
{"t":"2026-11-02T04:05:06.789Z","level":"info","svc":"magpie","ver":"3f7499a9ddd0","pod":"magpie-7c9…",
 "msg":"turn ended","request":"0190f…","conversation":"9b1…","turn":"t_…","sandbox":"…","person_key":"u3a…",
 "route":"POST /v1/conversations/{id}/turns","status":202,"ms":41,"error":null}
```

- Field names are fixed across services (`conversation`, never `conv` or `conversation_id`), so one filter works on
  every component. `msg` values are stable strings, listed per component in each spec (magpie 18.4, hermit 25,
  pigeon 19, supervisor 21.1); a new message is added, never reworded.
- Levels: `error` (needs an operator), `warn` (a refusal or a retried failure), `info` (lifecycle and one line per
  request that changed state). No debug level in production builds.
- Never logged: emails (only `person_key`), message text, prompts, file names and contents, tokens, cookies,
  view-as tokens, image data, secret values, SQL parameters.
- **Redaction** (`aviary-core::log::redact`, the Rust port of jarvis3 `common/log.ts` redact(), A3.F54, A7.F39): the
  subscriber applies it to every string field (`msg`, `error`, `message`, `detail`, any `&str`/`String` value) before
  writing; the typed fields (`request`, `conversation`, `turn`, `sandbox` as `Uuid`; `person_key`; `route`; `status`;
  `ms`) are written as they are. A library error that embeds a URL (`reqwest`) or a connection string (a
  `tokio-postgres` connect error naming the database URL) is therefore safe to log as `error`. Rules, applied in order,
  each with a unit test in `aviary-core` (`log::tests::redact_*`):
  1. `Bearer <x>`, `Basic <x>` (any case) → `Bearer [redacted]`, `Basic [redacted]`;
  2. userinfo in a URL, `scheme://user:pass@host` or `scheme://token@host` → `scheme://[redacted]@host` (covers
     `postgresql://magpie:…@…`, the sandbox proxy URL `http://sbx:hst1.…@10.202.1.1:3128`);
  3. query or form parameters whose name contains `token`, `key`, `secret`, `password`, `sig` or `signature` (any case:
     `sig=`, `X-Amz-Signature=`, `api_key=`) → `<name>=[redacted]`;
  4. any other run of 24 or more characters from `[A-Za-z0-9_\-+/=.]` → `[redacted]`, except a UUID (8-4-4-4-12 hex,
     kept: conversation and request ids stay searchable), a `person_key` (`u` + 24 hex) and an RFC 3339 time;
  5. hermit's token prefixes `hst1.` and `hct_` → the whole token `[redacted]` (rule 4 already catches them; the test
     pins it).
  A test asserts the output of each rule and that UUIDs, person keys and times survive. egress and tools keep jarvis3's
  redact() and egress's `safeUrl` (17.2 item 10), which apply the same rules.
- Where they live: container stdout, read with `kubectl logs` (the cluster's kubelet rotation keeps a few tens of MiB
  per container) and gone when a Pod is replaced. Everything an operator needs after the fact is therefore also a row
  in its owner's database: turn errors and their codes (magpie events), sandbox starts, releases, host events and
  admin actions (hermit), proxied requests and model calls with cost (egress), tool calls (tools). No log store is
  added in this refactor.

### 15.2 Tracing one conversation

`claude-env/.claude/skills/yyc-ai/trace.py <conversation-id> [--since 6h]` (rewritten from today's debugging steps,
gap-2 2.13): `kubectl -n yyc-ai logs -l app in (pigeon,magpie,hermit,egress,tools) --since … --prefix` through the
cluster ssh, keeps JSON lines whose `conversation` matches; adds magpie `GET /v1/admin/conversations/{id}` (events
with codes, no contents unless `--content`, which is audited), hermit `GET /v1/admin/sandboxes?conversation=<id>`
(starts, phases, releases, the stored console tail of 15.4), egress `GET /v1/admin/egress-log?conversation=<id>`; all
through `kubectl port-forward` with the admin pair tokens from the store; prints one time-ordered list.

### 15.3 The ops page and its contracts

hermit (YYC implementation) serves `/ops/` and `/ops/snapshot?tz=` (hermit spec 18), pigeon forwards admins' browsers
(pigeon spec 5.11), the news-stream monitor calls the snapshot directly with its pair token. The snapshot's field
contract with `monitor.py ai_slim` is hermit spec 18.3's list, kept name for name and unit for unit (camelCase), and
guarded by a golden test in hermit's CI and by E16 in the harness.

What hermit reads to build it (the platform's paths, R19/R20; shapes are camelCase):

```
GET http://magpie/v1/ops/people?tz=<IANA>          bearer HERMIT_TOKEN_MAGPIE
200 {"tz": "Asia/Hong_Kong",
     "people": [{"owner": "<email>", "total": 12, "today": 1, "last7d": 4, "live": 0, "deleted": 2,
                 "purged": 0, "lastActiveAt": "<RFC 3339>" | null}]}

GET http://magpie/v1/ops/view-as?limit=100&before=<RFC 3339>
200 {"viewAs": [{"at", "conversationId", "event": "start" | "send" | "create", "admin", "user", "session", "chars"}]}
     (admin and user are emails; chars = characters of the text sent while viewing; 13 in magpie)

GET http://magpie/v1/ops/stats
200 {"conversations", "deleted", "events", "rows", "ccBytes", "blobs", "blobBytes", "stagedBlobs", "stagedBytes",
     "outboxPending", "oldestOutboxAt", "oldestDuePurgeAt", "hermitFeedLagSeconds"}

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

Every `owner` and `user` above is the person's trimmed, lowercased email (the monitor's contracts read emails, hermit
18.3); `person_key` never appears in these shapes. magpie 16.4 and hermit 18.4 quote these shapes exactly; `live` in
magpie's `people` is magpie's count of conversations with a turn in progress, and hermit's snapshot uses its own
sandbox count for `people[].live` instead (hermit 18.3). Each call 5 s timeout; a failed section is `null` with its
message in `errors` (hermit spec 18.3). hermit's `service` section lists `ops.deployments` from its config: `pigeon,
magpie, hermit, egress, tools, cloudflared`.

### 15.4 Sandbox console tails (new, hermit's YYC implementation)

AWS hosts are terminated after their sandboxes leave, and with them `/var/log/sbx/<name>.log` (the supervisor's and
the keeper's JSON lines, supervisor spec 21.1). hermit therefore stores the last 64 KiB of each sandbox's console log
when it releases or loses it: sandboxd gains `GET /v1/sandboxes/{n}/log?tail=65536` (bytes, `text/plain`, at most
64 KiB; also served for 10 min after the sandbox ended), hermit writes `sandbox_logs(sandbox_id uuid primary key,
captured_at timestamptz, bytes bytea)` (14 days, janitor), served at `GET /v1/admin/sandboxes/{id}/log` (admin only).
For a lost host nothing can be fetched; the row says so (`bytes` null, `captured_at`, reason).

### 15.5 The news-stream monitor (DE0CH/YYC `news-stream/monitor.py`, in the switch commit)

```python
HERMIT_OPS_URL = os.environ.get("HERMIT_OPS_URL",
                                "http://hermit.yyc-ai.svc.cluster.local/ops/snapshot?tz=Asia/Hong_Kong")
def ai_snapshot():
    tok = os.environ.get("HERMIT_OPS_TOKEN")
    if not tok:
        raise RuntimeError("HERMIT_OPS_TOKEN (YYC AI hermit's monitor token) is not in the monitor pod's env")
    return get(HERMIT_OPS_URL, {"Authorization": "Bearer " + tok}, timeout=15)
```

`JARVIS_OPS_URL`, `JARVIS_OPS_USER` and the `x-jarvis-user` header are deleted (the monitor token has its own
read-only scope; Deyao no longer has to stay in `admins` for the dashboard to work, gap-2 2.29). `ai_slim` is
unchanged except its `source` string ("YYC AI operator snapshot (hermit /ops/snapshot: …)"); `ai_activity` is
unchanged, and so is its cursor `live:ai:cursor`: hermit 18.3's contract 2 keeps every field it diffs (`starts.list`
`what` = `start` | `release`, `owner` = email, `at` with three fraction digits and `Z`, `events` the 60 newest,
`egress.recent`, `budget.byUser`/`byModel` call counts, `health`), and imported rows keep their original `at`, all older
than the cursor, so the switch produces no duplicate and no missing card stream (hermit 18.3). `monitor.yaml`'s env and
its `yyc/secrets-rev` follow; hermit's ingress policy admits the monitor (10.2). The news-stream fixtures that fake the
snapshot change in the same commit: `tests/test_live_dashboard_page.py:131` (`kind: "hub"` → `"gateway"`, host
`yyc-sbx-gw` → `hermit-gw`, deployments `web`, `ui` → `pigeon`, `magpie`, `hermit`) and the other A19.F25 fixtures
(hermit 21.4).

### 15.6 After every deploy

1. `kubectl -n yyc-ai rollout status deploy/{pigeon,magpie,hermit,egress,tools,cloudflared} --timeout 10m`.
2. The ops snapshot's `health.state` is `ok` (or `warming` settling to `ok` within 5 min).
3. pigeon spec 18.4's `paseo-chat-check.mjs` against the live host as bot@deyaochen.com: create, stream to
   `turn_completed`, upload, history, share, delete for good. Recorded in the task record.
4. After an egress change: in that check's sandbox, `pip download --no-deps -d /tmp/p requests` and `npm view
   left-pad version` succeed through the mirror (`PIP_INDEX_URL`/`npm_config_registry` on the VIP with the proxy env
   set: the self-host path of 3.11.1), and `curl -sI https://<a denied host>/` is refused with egress's allowlist text (a
   host the allowlist denies, e.g. one from `denyHosts`).

### 15.7 Metrics

Every service exposes `/metrics` on 9090 (names `<svc>_*`, lists in each spec). YYC's cluster runs no metrics
scraper today and this refactor adds none: the numbers an operator needs live are in the ops snapshot (hermit) and
the per-service admin stats; `/metrics` is read with `kubectl port-forward` when investigating, and is ready for a
scraper later without code changes. The HPA uses metrics-server's CPU numbers (present in the cluster).

---

## 16. Resource sizes and the capacity budget

### 16.1 Pods (memory request = limit, the cluster's practice)

| Pod | CPU request / limit | Memory | Ephemeral | Basis |
|---|---|---|---|---|
| pigeon | 100m / 1 | 384Mi | 128Mi + app emptyDir 512Mi | sockets and pollers in memory, uploads streamed (pigeon spec 13 says 128-512; equal request and limit at the middle) |
| magpie | 200m / 2 | 1536Mi | 64Mi + `/tmp` 1Gi | blob LRU 256 MiB + up to 4 concurrent 40 MP decodes (≈160 MB RGBA each, plus resize buffers) + about 150 MiB base |
| hermit | 100m / 1 | 384Mi | 64Mi + `/tmp` 256Mi | hermit spec 21.1 (256-384); `juicefs quota` subprocesses |
| egress | 100m / 1 | 256Mi | cache 6Gi + `/tmp` 256Mi | today (OOM at 128Mi on 2026-10-28, gap-0 12) |
| tools | 50m / 1 | 128Mi | `/tmp` 256Mi | today |
| cloudflared | 20m / — | 64Mi | — | today (19 Mi measured) |
| JuiceFS mount pod (CSI, kube-system) for `hermit-jfs` | 50m | 192Mi | — | today's mount pods |
| hermit-gateway DaemonSet (main container) | 5m | 16Mi | — | a sleep loop; sandboxd itself is a host systemd unit with `MemoryMax=256M` |
| hermit-overlay-route DaemonSet | 5m / — | 32Mi request, 96Mi limit | — | today's `sbx-overlay-route` |
| sandbox (per conversation, hermit.json) | 1 CPU | 1Gi | 5 GiB `/tmp`; `/workspace` 1 GiB / 1,000 files quota | today; supervisor ≤ 64 MiB and keeper ≤ 4 MiB inside it |
| `magpie-pg` instance (×3, namespace `db`) | 250m / — | 1Gi | 10 Gi volume each | magpie 18.2 |
| `magpie-import` (switch only) | 200m / 1 | 1Gi | `/tmp` 2Gi | the largest transcript is 87 MB, streamed line by line; blobs streamed (cutover 7.2) |
| import CronJobs (switch only) | 100m / 500m | 256Mi | `/tmp` 256Mi | row copies in batches |
| harness runner (CI) | the hosted runner (4 vCPU, 16 GiB) | | | k3d with 3 nodes, 6 sandbox Pods at 1 GiB |

### 16.2 Cluster headroom

At minimum replicas the new namespace requests about 5.4 GiB (pigeon 0.75, magpie 3.0, hermit 0.75, egress 0.5,
tools 0.25, cloudflared 0.13 plus the mount pod); during the overlap the old `jarvis` stack (about 4.3 GiB of
requests, mostly `ui`'s 2 × 1.5 GiB) runs beside it. Live use on 2026-11-02: the two cpx32 nodes at 28 % and 47 %
of 8 GB, the apps cx33 at 45 %, the five cx23 at 51-63 % of their 2.7 GiB allocatable. magpie's 1.5 GiB Pods fit on
any of the 8 GB nodes and on a cx23 alone; the hostname spread places the two replicas apart. After teardown the
new stack uses about 1.1 GiB more than today's (magpie replaces `ui`'s 3 GiB and `web`'s 0.5 GiB; the conversations
no longer live in per-person Node processes). `magpie-pg` adds 3.4 GiB (three 1 GiB instances plus 128 Mi sidecars)
on three different `pool=apps` nodes (required anti-affinity), which fit beside the new stack's 5.4 GiB in the 13.9 GiB
free read on 2026-11-02 and leave the 2 Gi poller rollout slot (pg-ha 4). No node is added: Cluster `pg` keeps its one
instance on `db-worker1` (decision 10).

On two clusters (decision 13) the new stack and `magpie-pg` land on the new cluster, not beside the old stack: the
new cluster needs 13.5 GiB of requests free for YYC AI, plus each node's own pods, on at least three nodes in network
`yyc` during the window (DESIGN 15.7 and N12, which itemise it: three new cpx32 in fsn1, EUR 127.764 gross a month),
and the old cluster keeps its whole peak, 31.1 GiB on 2026-11-03, including the A1 surge, until A6 (DESIGN 15.2 N10). The
paragraph above is the one-cluster count of 2026-11-02.

### 16.3 Postgres connections

Section 11.4: at most 194 for the new stack at full scale (`magpie-pg` 106 of its 150; `pg` 88); `pg` 170 of 300
including today's other clients.

### 16.4 Storage and cost

New recurring cost, exact (Hetzner API `GET /v1/pricing`, 2026-11-02, EUR, VAT 20 %; pg-ha 8):

| Item | Net / month | Gross / month |
|---|---|---|
| `magpie-pg` (decision 2): three 10 GB volumes (3 × 10 × 0.0572) | 1.716 | 2.0592 |
| **Total** | **1.716** | **2.0592** |

Invoice line: EUR 2.06 a month with VAT; the cluster's Hetzner servers and volumes go from EUR 156.06 to EUR 158.12 a
month. Cluster `pg` adds nothing (decision 10: single instance; the declined HA plan was EUR 98.904 a month, two cpx32
EUR 85.176 and two 100 GB volumes EUR 13.728). Nothing else: the bucket fits in the Hetzner project's included storage (about 150 MB, magpie spec 18.5); hermit's,
egress's and tools' databases add under 50 MB; JuiceFS and the AWS hosts are unchanged; the deploy tunnel and its
Access app cost USD 0.00 (Zero Trust Free); GitHub Actions minutes are free for public repositories, and DE0CH/YYC's
(private) `yyc-ai.yml` runs draw on the account's included minutes, measured after the first runs against the
plan's allowance (deploy-workflows 9, last row); the new Cloudflare tunnel `pigeon` is free.

---

## 17. DE0CH/jarvis3: what it keeps, what it loses, what egress and tools must change

### 17.1 Keeps and loses

| Path | After the refactor |
|---|---|
| `chat/src/egress/**`, `chat/src/tools/**` | kept (egress = the YYC sandbox gateway implementation, tools = YYC's read-only MCP) |
| `chat/src/common/**` | kept, trimmed to what egress and tools use (config split, own databases, budget, log, hermit token lookup); `schema.ts` loses every table they do not own |
| `chat/egress-allowlist/**`, workflow `egress-allowlist.yml` | kept |
| `chat/Dockerfile`, image `ghcr.io/de0ch/jarvis3-chat` | kept; entrypoint dispatch by argv (`egress`, `tools`); public package |
| `chat/test/{egress,tools,budget,modelbody,common}.test.ts`, `chat/test/fakes` | kept and extended (17.2) |
| workflow `chat.yml` | kept: tests, the `jarvis3-chat` image, plus the aviary conformance job (17.3); the sandbox image and E2B template jobs removed |
| `chat/src/web/**`, `chat/src/main.ts` role `web` | deleted (ported into hermit and, for the UI glue, into magpie/pigeon) |
| `chat/test/{web,vm,packing,ops,sandbox,sandbox-k8s,e2b}.test.ts` | deleted (ported into hermit's tests, hermit spec 24.2) |
| `chat/sandbox/**` | deleted (aviary `images/sandbox`) |
| `chat/sandboxd/**` (daemon, host, aws, gvisor-patches, proto), workflow `sandboxd.yml` | deleted (aviary `crates/sandboxd`, `fabric/`; `proto/` is historical and not moved) |
| `deploy/chat/**` | deleted (YYC writes its own manifests, 9.7-9.9); `deploy.yml`'s line rendering it removed |
| `docs/CLAUDE-CODE.md`, `docs/SANDBOX-VM.md`, `docs/SANDBOXD-MEMORY.md` | deleted (aviary `docs/`, the component specs, YYC's README) |
| `docs/CHAT.md` | rewritten to describe egress and tools only |
| `server/`, `session-image/`, `integration/`, other `deploy/*`, `infra/` | Jarvis 3's own product, untouched |

These deletions land in the teardown task (18.4), after the old stack in `jarvis` is gone; until then the old image
tag the legacy manifests pin keeps working (images are immutable), so jarvis3 `main` may change earlier without
affecting the old stack.

### 17.2 What egress and tools change (jarvis3 work, specified here because no other spec owns them)

egress (the YYC implementation of 3.11's HTTP part):
1. **Role by argv**: `node src/main.ts egress` (`args: [egress]`); `JARVIS_ROLE` removed.
2. **Own configuration** `/etc/egress/egress.json` (ConfigMap `egress-config`), the egress parts of today's
   `config.json` plus what egress used to read from the sandbox section or env:
   ```json
   {
     "product": {"name": "YYC AI"},
     "models": [{"id": "deepseek/deepseek-v4.1-flash-1017", "label": "…", "reasoning": "…"}],
     "defaultModel": "deepseek/deepseek-v4.1-flash-1017", "smallModel": "deepseek/deepseek-v4.1-flash-1017",
     "titleModel": "deepseek/deepseek-v4.1-flash-1017", "titleReasoning": "none", "contextTokens": <the model's context window>,
     "routes": [
       {"name": "model", "upstream": "https://openrouter.ai/api", "model": true,
        "inject": {"Authorization": "Bearer ${OPENROUTER_API_KEY}"}, "allowPaths": ["/v1/messages"]},
       {"name": "tools", "upstream": "http://tools", "allowPaths": ["/mcp"],
        "inject": {"Authorization": "Bearer ${EGRESS_TOKEN_TOOLS}"}, "timeoutSeconds": 180}
     ],
     "selfHosts": ["10.202.1.1", "egress", "egress.yyc-ai", "egress.yyc-ai.svc", "egress.yyc-ai.svc.cluster.local"],
     "generatedAllowlist": true, "defaultAllowedHosts": ["…"], "denyHosts": ["…"], "blockCidrs": [],
     "maxUploadBytes": 8388608, "cacheGiB": 5,
     "budget": {"limitUsd": 100, "since": "2026-10-21T02:05:52Z", "ownerName": "Deyao"},
     "exit": {"socks": "socks.egress.svc.cluster.local:1080"}
   }
   ```
   (the values are today's `config.json` ones, whose shortlist is the one model shown; `label` and `reasoning`
   are carried over from it, `contextTokens` (new, R18) is the model's context window from OpenRouter's model list; `smallModel` is new: the id Claude Code uses for
   its small fast calls, published as `small` by R18, today the same model). `routes` keeps today's route shape and holds the routes of the fixed
   names `model` and `tools` (3.11.3), then the configured routes (3.11.8: YYC `labels`, with `allowPaths` and
   `identityHeaders`); the internal names `magpie` and `hermit` are built in (always
   `/v1/sandbox/**` to `http://magpie` / `http://hermit` with `EGRESS_TOKEN_MAGPIE` / `EGRESS_TOKEN_HERMIT`), not
   configurable. `selfHosts` replaces `EGRESS_SELF_HOSTS` and the shared config's `sandbox.vm.egressVip` (A6.F10):
   proxy-form requests whose host (any port) is listed are served locally (the mirror), always together with
   `127.0.0.1` and `localhost`; the harness's file lists its own VIP. `blockCidrs` replaces `EGRESS_BLOCK_CIDRS`.
   allowedModels = `models[].id` ∪ `defaultModel` ∪ `smallModel` ∪ `titleModel` (A6.F15). `JARVIS_CONFIG` removed.
   A `budget` edit rolls egress only (the hashed ConfigMap), not every component (gap-2 5.7).
   `exit` (Deyao, 2026-11-04; jarvis3 6c881c1, docs/CHAT.md "The exit"): every internet connection and the model
   route's OpenRouter go out by name through YYC's yyc-socks pool (news-stream/deploy/apps/socks.yaml), whose `sandbox`
   profile does the threat DNS and the public-address check over the nodes' IPv6; egress then resolves nothing itself
   (the threat-DNS and AddressGuard steps of 3 below are the exit's). The egress pods' NetworkPolicy needs
   `10.0.0.0/16` on TCP 1080 (the pool's endpoints are node addresses), as yyc/deploy/db.yaml `tools-to-socks`.
3. **The gateway contract** of 3.11, in its order (3.11.1): credential (signed token checked locally, liveness from
   `GET http://hermit/v1/sandbox-tokens/{sandboxId}` with `EGRESS_TOKEN_HERMIT`, cached 30 s, stale on error, 3.11.2;
   today's direct read of `sandboxes` goes), then the fixed names, then `selfHosts`, then today's internet chain
   (port, HostPolicy, threat DNS, AddressGuard, upload cap; A6.F2-F8, unchanged in behaviour and texts). The upload cap
   counts only internet CONNECT and forward traffic (3.11.4). Fixed names `magpie`/`hermit` write no `egress_log` row
   (aggregate counters `egress_fixed_requests_total{route,status}` and `egress_fixed_bytes_total{route,dir}` instead);
   `model`, `tools`, the mirror and internet requests keep one row each. Deleted: the `/x/<route>/…` paths, the
   `/wire` WebSocket and the E2B code (A6.F21), the `kv egress_pod:*` announcements (hermit reads EndpointSlices,
   hermit 13.8), `/x/sent-files` and `JARVIS_SENT_FILES_TOKEN`.
4. **In-cluster API on 8082**: `GET /v1/aviary/models` (callers `pigeon`; also served to sandboxes as
   `http://model/v1/aviary/models`) → `{"default", "small", "title", "contextTokens", "models": [{"id", "label",
   "reasoning"}]}`; `GET /v1/ops/summary` (caller `hermit`, 15.3); `GET /v1/admin/egress-log?conversation=&since=&limit=`
   (caller `admin`) → `{"rows": [{"at", "kind", "method", "host", "path", "status", "decision", "bytesUp", "bytesDown",
   "ms", "costUsd", "owner", "conversationId", "sandboxId"}]}`. Health on 9090: `/healthz` (event loop), `/readyz` (503
   until the allowlist is loaded and migrations are verified, and while draining), `/metrics` (today's counters plus
   `egress_drain_cut_total{kind}`, `egress_log_buffered`, `egress_log_dropped_total{kind}`).
5. **Own database `egress`** (`EGRESS_DATABASE_URL`): `egress_log` (today's columns + `person text` (email),
   `request text`, `row_id uuid` unique: the writer's idempotency key, so a batch retried after an insert of unknown
   outcome is written once), `budget_state`; migrations of its own tables only, under its own advisory lock (A6.F27). No
   retention on `egress_log` while `budget.since` is older than its oldest row (the budget sums every model row since
   then, A13.F6).
6. **One-off `egress import-jarvis`** (CronJob, 9.12): copies `egress_log` (every row, idempotent by source id), with
   `person` backfilled from `jarvis.sandboxes.owner` by `sandbox_id`, else `jarvis.conversations.owner` by
   `conversation_id` (hermit spec 22.4, the 189 historic calls), else null; re-run every minute until the old stack
   has drained; then the command is deleted.
7. **Drain** exactly as 3.11.6 (A6.F26, A7.F24, A16.F8): no new connections, `Connection: close` on kept-alive ones,
   model streams and CONNECT tunnels finish up to 600 s, fixed-name requests up to 60 s, the `tools` route up to its
   180 s timeout (A16.F9); `EGRESS_DRAIN_SECONDS` removed (the bounds are code, not deployment configuration). Manifest:
   readiness `/readyz`, preStop 10, grace 625 (9.3).
8. **A database outage never stops a running turn**: the budget gate (A6.F17) serves its last known status while the
   database is unreachable, adding the model costs this Pod has sniffed since (A6.F18) so a pod does not overspend
   on stale data; the log writer (A6.F22) keeps unflushed rows in memory, at most 50,000 (about 15 MB), dropping the
   oldest non-model rows first when full (counted in `egress_log_dropped_total`) and never a model row; rows are
   flushed in order when the database is back. Model and internet requests answer exactly as with the database up.
9. **Kept literals** (the ZDR guard, 13.6): `export const FORCED_PROVIDER = { zdr: true, data_collection: "deny" }` in
   `chat/src/egress/modelbody.ts`; `const isModel = route.model === true;`, `const rewrite = isModel && req.method !== "GET" && req.method !== "HEAD" ? new ModelBodyRewriter(`, `rewrite.push(chunk)`, `rewrite.end()` and `if (!rewrite) {` in
   `chat/src/egress/proxy.ts`; the test string `zero data retention is forced into every request body` in
   `chat/test/egress.test.ts`. Moving any of them is a change in the same pair of commits (jarvis3 and the YYC guard).
10. Logging unchanged (JSON lines, `redact()`, `safeUrl` for URLs, A3.F54, A6.F41), plus `person_key`, `request` and
    `conversation` fields under the names of 15.1.
11. Passes `aviary-conformance gateway` and `model` (17.3), and egress's own tests for the YYC parts: a `pip download`
    and an `npm view` through `http://sbx:<token>@<vip>:3128/mirror/…` with the proxy env set, against a fake pypi and
    npm upstream (the self-host path, A6.F9-F10), and the upload cap counting a CONNECT tunnel but not 40 MiB of
    fixed-name PUTs on one connection.

tools:
1. `args: [tools]`; own config `/etc/tools/tools.json` (today's `tools.*`: exa, anyapi, tikhub, capture, upstreams;
   A7.F9), catalogue at `/etc/tools/catalogue/catalogue.json`; the upstream listing identity named in its config
   (`upstreams[].listAs`), not `admins[0]` (gap-2 2.1, A6.F37).
2. **Caller and owner**: accepts only `TOOLS_ACCEPT_GATEWAY` bearers; the owner is `Aviary-Sandbox-Person` (the
   lowercased email, 3.11.3), which is what `${USER_EMAIL}` expands to in upstream headers, so yyc-mcp still gets
   `X-YYC-User: <email>` (A19.F9; yyc-mcp checks it against the catalogue) and `tool_calls.user_email` is filled as
   today. The sandbox's state is checked as today (only `starting`/`ready`, A6.F29) from hermit's liveness answer
   (`GET http://hermit/v1/sandbox-tokens/{sandboxId}`, `TOOLS_TOKEN_HERMIT`, cached 30 s, stale on error), never from
   a database. `Mcp-Session-Id` handling with yyc-mcp is unchanged (A19.F10).
3. **The yyc-mcp rate limit: decision** (A19.F11). The one bucket of 120 calls per minute shared by all YYC AI staff
   under the `yyc-ai` service identity is **kept unchanged**: yyc-mcp's live 7-day log shows 35 calls through the
   service token (supervisor 23), far below the limit; a 429 already reaches the model as a tool error and
   `tool_calls` records it, so a per-person split (a yyc-mcp change in the news-stream repo) waits until 429s appear
   there.
4. The MCP server name the model sees is `tools` (tool names `mcp__tools__*`, supervisor spec 6.4).
5. Own database `tools` (`tool_calls`), `tools import-jarvis` like egress's; health on 9090 (`/healthz`, `/readyz`,
   `/metrics`); drain unchanged (150 s, grace 180, A6.F38).

### 17.3 jarvis3 CI

`chat.yml`: `test` (as today, Postgres 17.11), `image` (`jarvis3-chat:<sha12>`), and `conformance`: starts the image
under test as egress with a test `egress.json` (upstream model → the suite's fake, allowlist deny-by-default) in a
Docker network where the suite's fake listener answers to the aliases `hermit`, `magpie`, `tools`, `upstream-model`,
then runs `aviary-conformance gateway` and `model` from `ghcr.io/de0ch/aviary-testing:<sha12>`, the sha12 read from
`chat/aviary.ref` (bumped when the gateway contract changes). The image is pushed only when all three pass.

---

## 18. The switch, platform view

The switch-over's text of record is DESIGN 9 (decisions 9, 11 and 12; the mechanisms of `switch-final.md` it
cites), carried out by cutover 10-11; its phases are A0-A7 (DESIGN 9.7). The platform's parts, in that order:

1. **A0:** aviary workspace, CI green, first images published and made public (5.5); jarvis3's egress and tools
   changes (17.2) as a new `jarvis3-chat` tag; YYC store keys of 12.3; the `legacy/` folder move on the old
   cluster's Flux (25.2); after the Talos session's T1-T2, the new cluster's footing and deploy path
   (deploy-workflows 12.5, DESIGN 15.2). Cluster `pg` is not changed (decision 10).
2. **A1:** P1's commits (cutover 10.1): `magpie-pg` and `pg-shared` on the new cluster, the roles and databases on
   `pg` (11.1), the bridges (25.3), everything in `yyc-ai`/`yyc-ai-fabric` on the new cluster (section 9; the deploy's
   first plan: creates only), the sealed Secrets (new cert), the tofu of 14.1, the cross-cluster policies (10.2),
   hermit's fabric prerequisites; the window pieces (`jarvis/ui-from-bridge`, `pigeon-to-ui`, `jarvis/ui-to-pigeon`,
   the importer's old-cluster ServiceAccount, the window keys of 12.3); the one ordinary ui rollout carrying the
   router's switch module and arming the freeze, applied by Flux paused and gated by the runner with Kustomization
   `yyc-ai` suspended while both old pods hold no client connection, child or running state (DESIGN 9.6, 15.3).
3. **A2:** the importers of 9.12 (Jobs, CronJobs) and rehearsals; acceptance on the test host (pigeon
   18.4, hermit 24.3, supervisor 22, a rollout of each new Deployment during a live turn).
4. **A3:** the switch commit (14.2; every catalogue reader on `catalogue.json`, R40; the monitor, 15.5) and
   Deployment `magpie-import` (9.12); the move of every open tab and the old connectors to 0 (DESIGN 9.3).
5. **A5:** the old stack to zero, one commit per step (cutover 11.1 `retire-old`).
6. **A6** (after P4): `import-done`; the teardown commits: on the old cluster, delete `yyc-ai/legacy/` except
   `legacy/catalogue/` (Flux Kustomization `yyc-ai`, prune true, deletes namespace `jarvis`'s objects, PV
   `yyc-ai-jfs`, RuntimeClass `gvisor`, `kube-system/sbx-overlay-route`), then a `clusters/hetzner` commit removes
   Namespace `jarvis` (Flux `yyc-apps`); on the new cluster, the window objects (`magpie-import`, `jobs/`, `ui-legacy`,
   `pigeon-bridge`, the egress rules to the bridges) with `Deploy-Prune:` trailers (planned from the session first);
   `bridges.sh down` for the links that are gone (the server with the last; 25.3); also
   `yyc-ai-ami.yml`, `config.json` (no reader is left: R40), the legacy halves of the ZDR guard and `openrouter.py`
   (13.6), `infra/gvisor-node.sh` (A7.F53), the `jobs/` folder, `secrets.txt` lines of namespace `jarvis` and their
   sealed files, `pg-ingress`'s `10.201.0.0/24`, the yyc policies' `jarvis` peers; tofu 14.3; jarvis3's 17.1
   deletions; claudecodeui's router; aviary's importer and window code (a contraction release, pigeon first); 11.3
   (drop `jarvis`, `cloudcli` after their final dumps and the old chat UI's archive); store 12.3's deletions; docs
   and skills rewritten.
7. **P6** (+30 days): hermit's `legacy-cleanup`. **A7** (+90 days): the legacy-path set (cutover 10.7).

Rollback: DESIGN 9.10 and cutover 14 (per conversation, behind pigeon, until P4 ends; after P4 roll forward or pin
the previous aviary and jarvis3 tags, schemas only grow).

---

## 19. Failure modes of the platform

(Two-cluster rows: 25.7.)


| Failure | Effect | Handling |
|---|---|---|
| An aviary gate fails on `main` | nothing of that commit is published | fix forward; YYC can only pin tags that passed |
| A GHCR package left private | `yyc-ai.yml`'s `check` fails at step 4 before `deploy` runs | make it public (`github-web`), re-run the check |
| GHCR unavailable during a rollout | the surge Pod sits in `ImagePullBackOff`; `maxUnavailable: 0` keeps every old Pod serving | it resolves when GHCR returns; no action needed |
| A new version never becomes ready (bad config file, failed migration check) | the rollout stalls on one surge Pod; old Pods serve; `deploy` goes red at its rollout wait and prunes nothing | revert the YYC commit (with its `Deploy-Prune` trailers if the revert removes objects) |
| A resealed Secret not yet decrypted when Pods roll | prevented: the deploy waits for each changed SealedSecret's `Synced` before it applies workloads (deploy-workflows 6.4 step 2) | reseal and push if `Synced` never comes |
| `deploy` red (connect, deletion gate, rollout) | nothing new applies; running state is fine; the run summary names the step | a commit that dropped an object without its trailer gets a follow-up commit with the printed trailer; tunnel or Access down: the next push or the 3-hourly run retries; re-run after the fix |
| A commit drops an object by accident | `deploy` refuses before any write (no trailer) | restore the object or add the trailer |
| HPA wants a Pod and no node has room | Pending Pod; current Pods serve | the PriorityClass `serving` preempts lower-priority news-stream batch Pods if any; else add a node |
| `pg` restarts (single instance, decision 10) | 11.5's table: `pg-rw` and `pg-hosts` gone for the restart; hermit admissions answer `unavailable`, egress and tools keep serving, JuiceFS blocks and retries; Pods stay ready (3.2), so the Services do not flap | a stated gap (DESIGN 7.3 item 2): planned restarts outside working hours; a lost `db-worker1` is a restore from `pg-pitr` |
| `magpie-pg` primary restarts (operator or node update) | a switchover promotes a synchronous standby first: seconds of `unavailable` on cc writes, reports and polls, retried by the supervisor and pigeon; no acknowledged write is lost (`dataDurability: required`) | none; magpie's watchdog measures heartbeat silence only from `availableSince` (magpie 8.8), so no turn is killed for magpie's own outage |
| magpie down for minutes (a bad roll, both standbys gone) | agents' `~/.claude` writes block, turns pause, nothing is lost; pigeon shows conversations as they were | roll back the YYC commit; when magpie returns the watchdog does not fail paused turns (magpie 8.8), and an offset mismatch is repaired from the supervisor's cache (magpie 6.12) |
| hermit down or slow | new turns cannot get a sandbox (`unavailable`); running turns are untouched: the gateway verifies tokens locally and serves cached liveness (3.11.2), so model, tools and FUSE traffic keep flowing (E24) | roll back or fix; revocation is delayed by the outage (a removed person keeps a running sandbox until hermit is back plus 30 s) |
| A gateway (egress) rollout | each replica drains up to 600 s (3.11.6); nothing that finishes within that is cut; supervisor calls move to the other replica on `Connection: close` | none; `egress_drain_cut_total` counts what was cut |
| Connection exhaustion | new connections refused | the budget of 11.4 stays under 300 at full scale; `<svc>_db_pool_waiting` in `/metrics` |
| The bucket unreachable | uploads, image previews and blob reads fail with `unavailable`; text turns keep working | Hetzner status; nothing to do in the cluster |
| A protocol skew (a sandbox image speaking a number magpie no longer accepts) | prevented by `yyc-ai.yml` check step 4; if forced, that sandbox's calls get 400 `unsupported_protocol` and hermit recycles it | `POST /v1/admin/recycle` |
| A PDB blocks a node drain (2 replicas, one Pod per node) | the drain waits until the evicted Pod's replacement is ready elsewhere | expected; `minAvailable: 1` with 2 replicas lets one go at a time |
| The harness flakes in CI | a commit is not published | flakes are bugs: the failing scenario's artifacts (7.8) go into an issue; no automatic retry of `e2e` on `main` |
| The AMI bake fails | new hosts fetch the image from the gateway (about 45 s slower cold starts) | re-run the workflow; `infra.sh check` names the stale pin |

---

## 20. Requirements this spec places on the component specs

Everything in 3.10 (R1-R41) and 3.11, plus, per spec (each owner applies its own list; most of it is already in the
named spec after review round 1, listed so the specs end in one state):

- **All services:** `/version` on 9090 with `provides`/`accepts` (3.2), `Aviary-Protocol` on every client call,
  image labels (5.3), `aviary-core` for env, logs (with `redact`), health, drain and auth, `aviary-pg` for Postgres
  and its no-transaction-across-I/O rule (4.3).
- **magpie:** routes under `/v1/` in camelCase; the rows model of R27 (magpie 7.3-7.6, already written); `/v1/ops/*`
  exactly in 15.3's shapes (magpie 16.4 adds nothing beyond them); the hermit feed follower (R8); pool 12; readiness per
  3.2; `magpie import …` subcommands of the cutover 7.9 list (R15); `magpie-pg` (R31, decided); the R36 live-frame receiving side with 409 `turn_not_active`; the R37 refusal shape.
- **hermit:** routes under `/v1/` (R3, R4); env per 3.7 and no `HERMIT_DB_CA`; `hermit.json` gains `fabric.sandboxd`
  and `fabric.runsc` (9.2) and `infra.sh` reads its three pins from it (13.5); the gateway and overlay-route
  DaemonSets in `yyc-ai-fabric` (R24); sandbox console tails (15.4) and the sandboxd log route;
  `hermit catalogue-check` (13.3 step 7); `GET /v1/admin/sandboxes?conversation=` (15.2); the sandbox image built from
  `images/sandbox` with the keeper as PID 1 (R12); the R33 token keys and liveness routes; R40's switch-commit timing in
  21.5 (already); the golden test of both monitor contracts (hermit 18.3, 24.2), checked again by E16.
- **pigeon:** headers per 3.5 on its upstream calls; models from `GET http://model/v1/aviary/models` (R18); grace 320
  (9.3); 14.1.18 = magpie's rows (R27); no live frames (R36); R37 as revised (no retry loop and no sentences; the
  legacy upstream of switch-final 4.1 instead, DESIGN 12 D14); in 14 and 17.5 the ingress service `http://pigeon.yyc-ai.svc.cluster.local:80` (not `pigeon.jarvis.svc`) and
  R39's names (its `staff_ai_paths` already matches; output `tunnel_token_pigeon`); turn origin `person|agent|import` (R30; pigeon 22.1 item 3 still
  says `user|cli|import`).
- **supervisor:** `Aviary-Protocol`, `Aviary-Lease` header names; R32: every call to the fixed names goes through the
  proxy of its boot environment (`HTTP_PROXY`, credential in the userinfo), not direct to `http://<name>` via
  `/etc/hosts`, and there is no `HERMIT_SANDBOX_TOKEN` (supervisor 17.1, 23 hermit item 3 change; the keeper refuses
  to start without the proxy variables, hermit 5.1); R35's shapes; R36 (no frames in this release; 404 or 409 on
  `/live` = drop, never retry); supervisor 19 gains a "gateway rollout" row (fixed-name calls cut at a replica's 60 s
  bound are retried with the same `opId`; a cut model stream is Claude Code's own `api_retry`), and coverage row
  A0.F30's "gateway rollouts never touch turns" becomes "a gateway rollout cuts nothing that finishes within its
  bounds (platform 3.11.6)"; supervisor 23 tools: "the gateway sets `Aviary-Sandbox-Person`; tools maps it to
  `X-YYC-User`" (no `X-YYC-User` from the gateway).
- **cutover:** follow 9.12 for the importers (`magpie import handover` in Deployment `magpie-import` from the `magpie`
  image; no `magpie-import` image, binary or `magpie-import.yml`; CronJobs `*-import-jarvis` from the service images,
  egress/tools by argv `[egress, import-jarvis]`, not `JARVIS_ROLE`); PVC `hermit-jfs` read-only (not `jfs`); Secret
  names without `-env` (3.7); R39's tofu names (no `pigeon_paths`; `staff_ai_paths` shrinks in P5); R40's catalogue
  timing (P5.4 changes no reader); R41 (no jarvis3 remote base in P1's layout; YYC's own egress/tools/cloudflared
  manifests); R24 and R21 in P1 (`hermit-gateway` in `yyc-ai-fabric`, no `ghcr-pull`); R37's refusal shape in 6.4 and
  pigeon 14.1's `/v1` camelCase shapes for every magpie call (18.1); 18.2 points to R27; 17.4 points to the supervisor
  list above; the P0 checklist's ZDR item becomes the P1 items of 13.6 (guard green, `openrouter.py OUT` and
  `--verify` green before the switch); `magpie-pg` in P1 commit 1; the switch itself per DESIGN 9 (18).
- **The gateway (egress) and tools:** 3.11 and 17.2.

---

## 21. Open questions (Deyao only)

None left in this spec. Deyao's decisions of 2026-11-02 (`DECISIONS.md`) settled the former four: deploys by GitHub
workflow (decision 1, 13), `magpie-pg` (decision 2, 11.1), the licence AGPL-3.0-or-later (decision 3, 4.6) and
Cluster `pg` single-instance (decision 10, revising decision 8; 11.5). What remains for him is in DESIGN 14.

---

## 22. Coverage: inventory features no component spec owns

Every feature of `inventory.json` whose `target_home` is egress, tools, ops/deploy, apps, "each/every service" or a
home-less "drop" (137 features: the IDs the review listed, whose ranges expand to 137, not 131). Generated from
the inventory so none is missed; "where" is the first location the inventory gives. Dispositions: **kept** (unchanged
behaviour, possibly a new home for its settings), **changed** (the section that changes it), **moved**, **dropped**
(with the reason). The A4 rows and A17.F21 are the apps' and are covered in pigeon 21; they are listed here only so
this table is complete. Rows already covered in another spec's table point there as well.

| Id | Feature | Where | Disposition |
|---|---|---|---|
| A0.F17 | Process exit handling and crash restart | `router.mjs:516-523, 657 forget, test 'a process that …` | dropped: no per-person processes (pigeon 10); a crashed service Pod is restarted by Kubernetes, the supervisor by its keeper (supervisor 4.2) |
| A0.F18 | Process stop | `router.mjs:567-578` | dropped with the per-person processes; every service's stop is the drain of 3.3 |
| A0.F21 | Per-replica cap and LRU eviction | `router.mjs:73, 602, 898-899, 907-914` | dropped: no per-person processes; capacity is the HPA (9.3) and hermit's slots |
| A0.F27 | HTTP forward to a peer replica | `router.mjs:1160-1174` | dropped: any pigeon replica serves anyone (pigeon 10), so nothing is forwarded |
| A0.F29 | Hop-header loop guard | `router.mjs:97, 846-856, 1161, 1186` | dropped with the forwarding |
| A0.F32 | /healthz and /readyz | `router.mjs:945-950` | changed: every service serves `/healthz` and `/readyz` on 9090 (3.2); readiness depends on no dependency (decision 11), as today |
| A0.F36 | Structured logs (no metrics) | `router.mjs:126-128 log()` | changed: one JSON line format for every service (15.1), stable `msg` lists in each spec; `/metrics` added (15.7) |
| A0.F37 | Image build and router tests | `jarvis/Dockerfile:1-52` | changed: aviary `ci.yml` builds and tests every crate and gates publishing on the harness (6.1); router tests ported to pigeon 18.2; `jarvis-image.yml` deleted at teardown (18.4) |
| A0.F38 | Jarvis 4 runtime bundle | `jarvis/runtime-bundle.sh:1-59` | kept unchanged, outside this refactor: Jarvis 4's runtime bundle and its dogfooding stay open; a later Jarvis 4 sandbox image takes the binaries from the `aviary-<sha12>` release (decision 3) |
| A1.F13 | Provider auth always 'signed in' | `server/modules/providers/services/provider-auth.service.t …` | dropped: no host-side auth concept; the apps' provider status is a pigeon compat constant (pigeon 7.5) |
| A1.F48 | Logging | `server/modules/jarvis/*.ts (console.warn '[jarvis] …'), …` | changed: 15.1; today's `[jarvis] …` lines become component `msg` values (magpie 18.4, hermit 25, pigeon 19) |
| A3.F44 | Ops section: budget and spend | `web/ops.ts:414-450` | changed: egress serves the budget section of `GET /v1/ops/summary` (15.3, 17.2 item 4), `byUser` from `egress_log.person`; hermit's page shows it (hermit 18) |
| A3.F45 | Ops section: egress errors (24 h) | `web/ops.ts:453-479` | changed: the `egress` section of the same route (15.3); `owner` is an email |
| A3.F53 | Shared budget calculation | `common/budget.ts:1-71` | kept in egress: `budget` in `egress.json`, spend summed from egress's own `egress_log`, imported rows included (17.2 items 2, 5, 6) |
| A3.F54 | Logging and redaction | `common/log.ts:2-24` | changed: `aviary-core::log::redact()` with today's rules, applied by the logger itself, unit-tested per rule (15.1); egress and tools keep jarvis3's (17.2 item 10) |
| A3.F62 | Image and deploy pipeline for web | `docs/CLAUDE-CODE.md:184-192` | changed: the deploy flow of 13.2; sandbox image pinned in `hermit.json`; AMI by `hermit-ami.yml` (13.5); `secrets-rev` kept (13.4) |
| A4.F13 | Local timeline replica cache | `runtime/replica-cache/index.ts:64-73 (CachedTimeline` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F18 | Queue while a reply runs | `composer/index.tsx:186 (CHAT_SEND_BEHAVIOR='queue'), …` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F21 | New-chat send-ready gate | `screens/new-workspace/send-ready.ts:1-15` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F39 | Opening server files on iOS vs web | `yyc/open-file.ts:12-27` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F47 | Early message box before JS loads (web) | `public/index.html:152-213 (#yyc-early, inline script, …` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F48 | Page frame and staged loading | `public/index.html (#boot frame)` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F49 | Offline / unreachable screen | `app/welcome.tsx:14-30` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F50 | Routes / screens | `app/src/app/*: index.tsx, new.tsx, …` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F51 | Conversation menu | `screens/workspace/conversation-menu-items.tsx:17-60` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F52 | Localisation | `i18n/resources/en.ts:3151-3222 (conversation.*), …` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F53 | Branding / product name | `styles/theme.ts` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F54 | Drafts persistence | `stores/draft-store/state.ts:60-75` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F55 | Last conversation memory | `stores/last-workspace-selection.ts:8` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F56 | iOS build and TestFlight CI | `.github/workflows/yyc-ios.yml:1-100` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F57 | Web build | `scripts/build-paseo-ui.mjs` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A4.F58 | Developer local mode | `yyc/api.ts:12-16` | apps unchanged; pigeon 21 has its row (the A4 rows are pigeon's) |
| A6.F2 | Forward proxy: CONNECT tunnels | `chat/src/egress/proxy.ts:237-294` | kept for internet traffic (3.11.1 step 4); its drain changed (3.11.6) |
| A6.F3 | Forward proxy: absolute-URI plain HTTP (+ apt cache) | `chat/src/egress/proxy.ts:296-370` | kept, apt cache included; fixed names and self hosts are matched before it (3.11.1) |
| A6.F4 | #admit check chain and refusal texts | `chat/src/egress/proxy.ts:59-63 (denyText), 191-230` | kept for internet traffic, refusal texts unchanged; the token step is 3.11.2 (signed token, cached liveness) |
| A6.F5 | Sandbox host allowlist/denylist (HostPolicy) | `chat/src/egress/allowlist.ts:1-100` | kept; `generatedAllowlist`, `defaultAllowedHosts`, `denyHosts` in `egress.json` (17.2 item 2) |
| A6.F6 | Threat-intel DNS resolver | `chat/src/egress/threatdns.ts:1-63` | kept unchanged |
| A6.F7 | AddressGuard (public addresses only, anti-rebinding) | `chat/src/egress/netguard.ts:1-96` | kept; `EGRESS_BLOCK_CIDRS` becomes `blockCidrs` in `egress.json`; the VIP never reaches it (self host, 3.11.1) |
| A6.F8 | Per-connection upload cap | `chat/src/egress/proxy.ts:27-28, 88, 267-276, 358-367` | changed: counts only internet CONNECT and forward bytes; fixed names have per-request limits (3.11.4); `maxUploadBytes` in `egress.json` |
| A6.F9 | Package mirror pypi/npm with disk cache | `chat/src/egress/proxy.ts:374-457` | kept; reached through `selfHosts` (17.2 item 2); tested by egress (17.2 item 11) and after each deploy (15.6 item 4) |
| A6.F10 | Self-host detection (proxy-form requests to egress itself) | `chat/src/egress/proxy.ts:92-94, 172-186` | changed: `selfHosts` in `egress.json` (VIP 10.202.1.1 and the Service names) replaces `EGRESS_SELF_HOSTS` and `sandbox.vm.egressVip`; fixed names are routed first |
| A6.F11 | Credential-injecting routes /x/<route>/<path> | `chat/src/egress/proxy.ts:461-580` | changed: `/x/<route>/…` deleted; the fixed names `model` and `tools` are the routes, defined in `egress.json` `routes` with today's `inject`/`allowPaths`/`${VAR}` rules (3.11.3, 17.2 item 2) |
| A6.F13 | Forced ZDR body rewrite (zero-copy) | `chat/src/egress/modelbody.ts:9-68, 125-300` | kept unchanged on route `model`; its literals kept for the ZDR guard (17.2 item 9, 13.6) |
| A6.F14 | Reasoning/thinking level per model | `chat/src/egress/modelbody.ts:46-55, 70-82` | kept; `models[].reasoning`, `titleReasoning` in `egress.json`; the models route publishes `reasoning` (R18) |
| A6.F15 | Model shortlist enforcement | `chat/src/egress/proxy.ts:582-591` | changed: the shortlist adds `smallModel` (17.2 item 2) |
| A6.F16 | Model body memory budget (BodyBudget) and 64 MiB cap | `chat/src/egress/modelbody.ts:84-123` | kept unchanged (3.11.3) |
| A6.F17 | Shared budget gate (402 budget_exceeded) | `chat/src/common/budget.ts:1-71` | kept; during a database outage the gate serves its last status plus the Pod's own sniffed costs (17.2 item 8) |
| A6.F18 | Cost sniffing from model responses | `chat/src/egress/proxy.ts:535-553, 666-704` | kept unchanged |
| A6.F19 | Route `tools` (MCP passthrough) | `YYC config.json egress.routes[1]` | changed: fixed name `tools` (`http://tools/mcp`) with the gateway's pair bearer and `Aviary-Sandbox-Person` instead of the sandbox's bearer; route timeout 180 s (3.11.3-3.11.4); server name `tools` |
| A6.F21 | The wire (GET /wire WebSocket) for hosted sandboxes | `chat/src/egress/proxy.ts:126-156` | dropped with the E2B provider (17.2 item 3) |
| A6.F22 | egress_log table and batched writer | `chat/src/egress/egresslog.ts:1-61` | changed: own database, `person` and `request` columns, no rows for `magpie`/`hermit` calls (counters instead), in-memory buffer through a database outage (17.2 items 3, 5, 8) |
| A6.F24 | egress /metrics and /healthz (port 9090) | `chat/src/egress/main.ts:49-75` | changed: `/healthz`, `/readyz`, `/metrics` on 9090; readiness probe `/readyz`; drain and buffer counters (17.2 item 4) |
| A6.F26 | Zero-downtime egress rollout and drain | `chat/src/egress/main.ts:92-105` | changed: 3.11.6 (600 s for streams and tunnels, `Connection: close`), grace 625 s; hermit's EndpointSlice watch replaces the kv announcement |
| A6.F27 | Migrations run by egress at start | `chat/src/egress/main.ts:19-20` | changed: egress migrates only database `egress` (17.2 item 5) |
| A6.F28 | tools: MCP server (Streamable HTTP, stateless, JSON) | `chat/src/tools/server.ts:72-170` | kept unchanged; server name `tools` |
| A6.F30 | tool_calls table | `chat/src/tools/server.ts:80-99` | changed: own database `tools`, imported once and for the delta (9.12) |
| A6.F31 | web_search (Exa) | `chat/src/tools/builtin.ts:18-64` | kept unchanged; key in Secret `tools` (12.2), settings in `tools.json` |
| A6.F32 | web_fetch (own capture → Exa contents → direct guarded GET) | `chat/src/tools/builtin.ts:79-192` | kept unchanged; capture settings in `tools.json`; `yyc/web` admits `yyc-ai` tools (10.2) |
| A6.F33 | social_search (TikHub, 8 platforms) | `chat/src/tools/builtin.ts:194-231` | kept unchanged; settings in `tools.json` |
| A6.F34 | AnyAPI tools (anyapi_search/describe/run/result) | `chat/src/tools/builtin.ts:233-394` | kept unchanged; settings in `tools.json` |
| A6.F35 | Built-in tool availability by config | `chat/src/tools/server.ts:54-70` | kept; driven by `tools.json` |
| A6.F36 | Upstream MCP proxying (yyc-mcp) with read-only allowlist filter | `chat/src/tools/upstream.ts:1-160` | kept; `${USER_EMAIL}` = `Aviary-Sandbox-Person`, so yyc-mcp still gets `X-YYC-User` (17.2 tools item 2) |
| A6.F37 | Upstream tool-list refresh loop | `chat/src/tools/upstream.ts:446-460, 488-496` | changed: the listing identity is named in `tools.json` (`listAs`), not `admins[0]` (17.2 tools item 1) |
| A6.F38 | tools zero-downtime drain | `chat/src/tools/main.ts:27-39` | kept (150 s drain, grace 180); egress's route waits 180 s (3.11.4) |
| A6.F40 | Network isolation around egress/tools | `deploy/chat/network.yaml:1-58` | changed: section 10 (default deny both ways in `yyc-ai`, per-component policies, 3128 the only sandbox port) |
| A6.F41 | Logging | `chat/src/common/log.ts` | kept (JSON lines, `redact`, `safeUrl`), plus the 15.1 field names |
| A7.F0 | Flux GitOps for YYC AI (Kustomization yyc-ai) | `news-stream/deploy/clusters/hetzner/flux-sync.yaml:8-56` | changed: `yyc-ai.yml` applies `yyc/deploy` (13.1, deploy-workflows); Kustomizations `yyc-ai`, `yyc-ai-secrets` deleted with `deletionPolicy: Orphan`; job `check` (13.3) checks every PR and push |
| A7.F1 | Remote base pinned to a jarvis3 commit | `yyc/deploy/kustomization.yaml:9,26-33` | dropped: YYC writes egress's and tools' manifests and pins only the `jarvis3-chat` image (R41, 9.7) |
| A7.F2 | Instance ConfigMap jarvis-config (hashed name) | `yyc/deploy/kustomization.yaml:34-37` | dropped: per-implementation ConfigMaps `hermit-config`, `egress-config`, `tools-config`; none for pigeon or magpie (9.10) |
| A7.F3 | Access catalogue ConfigMap yyc/yyc-catalogue (unhashed) | `yyc/deploy/kustomization.yaml:38-43` | kept: `yyc/yyc-catalogue`, key `config.json`, sourced from `catalogue.json` in the switch commit (R40, 9.2, hermit 21.5) |
| A7.F9 | config.json: tools | `yyc/deploy/config.json:51-66` | moved to `tools.json` (17.2 tools item 1) |
| A7.F10 | config.json: budget | `yyc/deploy/config.json:67` | moved to `egress.json` `budget`; an edit rolls egress only (17.2 item 2) |
| A7.F24 | egress Deployment zero-downtime + VIP announcement | `deploy/chat/egress.yaml:1-103` | changed: drain 3.11.6; announcement replaced by hermit's EndpointSlice watch (hermit 13.8) |
| A7.F25 | tools Deployment zero-downtime | `deploy/chat/tools.yaml:1-192` | kept (9.3), readiness on `/readyz` |
| A7.F27 | Secrets rollout annotation yyc/secrets-rev | `yyc/deploy/kustomization.yaml:74-83` | kept, one annotation per Deployment (9.2, 13.4) |
| A7.F35 | NetworkPolicies (live: 14) | `deploy/chat/network.yaml:1-58` | changed: section 10 |
| A7.F36 | Namespace, LimitRange, PriorityClass | `deploy/chat/namespace.yaml` | kept for namespace `yyc-ai`, same LimitRange numbers and PriorityClass `serving`; plus `yyc-ai-fabric` (9.9) |
| A7.F39 | Structured logging | `chat/src/common/log.ts:1-25` | changed: 15.1 (one format, `redact` in the logger) |
| A7.F44 | Cloudflare tofu workflow (drift revert) | `.github/workflows/cloudflare.yml:1-80` | kept; path filter follows `catalogue.json` in the switch commit (hermit 21.5); the new tunnel's resources (14) |
| A7.F45 | jarvis3 chat workflow (images) | `.github/workflows/chat.yml:1-169` | changed: `chat.yml` keeps tests and the `jarvis3-chat` image, adds the aviary conformance job, loses the sandbox image and E2B jobs (17.3) |
| A7.F46 | jarvis3 egress-allowlist weekly refresh | `.github/workflows/egress-allowlist.yml:1-55` | kept unchanged; reaches YYC with the next `jarvis3-chat` pin (13.2) |
| A7.F49 | yyc-ios workflow (TestFlight) | `.github/workflows/yyc-ios.yml:1-126` | kept in claudecodeui (pigeon 21, A4.F56) |
| A7.F51 | Zero-data-retention guard (test + workflow) | `yyc/tests/test_zdr.py:1-67` | changed: rewritten against `egress.json` and the image tag (13.6) |
| A7.F52 | OpenRouter key and guardrail script | `yyc/infra/openrouter.py:1-125` | changed: reads `egress.json` with `smallModel`; key name `yyc-chat` kept (13.6) |
| A7.F53 | gVisor node script and inert RuntimeClass | `yyc/infra/gvisor-node.sh:1-56` | dropped: `infra/gvisor-node.sh` and the inert RuntimeClass deleted at teardown (18.4) |
| A7.F54 | Deploy-a-new-version runbook | `README.md:74-90` | changed: the deploy flow of 13.2; YYC README and the yyc-ai skill rewritten at teardown (18.4) |
| A10.F2 | /api/auth/* (status, user, refresh, logout) | `server/modules/auth/auth.routes.ts:18,44,48,52 (register …` | dropped: unknown `/api` paths answer 404 JSON (pigeon 5.12) |
| A10.F3 | GET /api/projects (+ /:projectId/sessions, toggle-star, … | `server/modules/projects/projects.routes.ts:61-76 (GET …` | dropped: the one project is a pigeon constant |
| A10.F6 | POST /api/assets/images, GET /api/assets/images/:filename, GET … | `server/modules/assets/assets.routes.ts:57-76, :98-128 …` | dropped: uploads use `/api/assets/files` (pigeon 5.7) |
| A10.F9 | Web Push / VAPID configuration at start | `server/index.ts:340 configureWebPush()` | dropped; VAPID rows are not imported (magpie 19) |
| A10.F10 | Notification orchestrator on run stop/fail/action-required | `server/modules/notifications/services/notification-orches …` | dropped (no channel was on) |
| A10.F11 | /desktop-notifications WebSocket | `server/modules/websocket/services/websocket-server.servic …` | dropped; not a signed-in path (14) |
| A10.F12 | /api/notifications/endpoints (notification channel endpoints) | `server/modules/notifications/notifications.routes.ts:42,6 …` | dropped |
| A10.F13 | /api/system (only /update, guarded) | `server/modules/system/system.routes.ts:11` | dropped |
| A10.F14 | /api/user preferences, onboarding, drafts | `server/modules/user/user.routes.ts:33 …` | dropped; rows not imported (magpie 21.1 decision 9) |
| A10.F15 | Queued-message dispatcher (session_drafts.queued_message) | `server/modules/scheduled-messages/services/scheduled-mess …` | dropped (magpie 21.1 decision 9) |
| A10.F16 | /api/scheduled-messages (list/create/delete) + scheduled dispatcher | `server/modules/scheduled-messages/scheduled-messages.rout …` | dropped (magpie 21.1 decision 9) |
| A10.F17 | /api/voice (health, transcribe, tts) | `server/modules/voice/voice.routes.ts:49,53,87` | dropped (voice is off in the apps) |
| A10.F23 | Upstream Paseo HTTP download path /api/files/download | `paseo/packages/app/src/stores/download-store.ts:106-151 …` | dropped; pigeon never serves `index.html` for an unknown `/api` path (pigeon 5.12) |
| A10.F29 | Jarvis 4 local-mode-only surface (same server) | `server/index.ts:135-139 (GET /api/jarvis/me local, …` | kept unchanged, outside this refactor (Jarvis 4 dogfooding stays open) |
| A12.F6 | projects table (/workspace project row) | `schema.ts:130-138` | dropped: a pigeon constant; not imported |
| A12.F16 | notification_channel_endpoints | `schema.ts:114-128` | dropped; not imported; gone with database `cloudcli` (11.3) |
| A12.F17 | api_keys / user_credentials / github tokens | `schema.ts:64-89` | dropped; not imported; gone with database `cloudcli` (11.3) |
| A12.F18 | provider_models (custom model catalogue) | `schema.ts:179-191` | dropped; models come from R18 |
| A13.F1 | confirmations / allowed_hosts (retired at schema 6) | `jarvis3 chat/src/common/schema.ts:46-51, :80-89` | dropped: not recreated in database `egress` (11.1); gone with `jarvis` (11.3) |
| A13.F2 | turns.presented column | `jarvis3 chat/src/common/schema.ts:90-92` | dropped with database `jarvis` |
| A13.F6 | Budget spend depends on full egress_log history (no retention) | `jarvis3 chat/src/common/budget.ts (budgetStatus), …` | kept: every row imported, no retention on `egress_log` while `budget.since` is older (17.2 item 5) |
| A13.F7 | Stale kv keys turn_sandbox:/replica:/presence: | `writers removed in jarvis3 07e2e49: @2156d8a …` | dropped with database `jarvis`; nothing in `yyc-ai` reads `jarvis.kv` |
| A13.F9 | ops_events kind 'budget' (audit of the removed admin budget override) | `written by jarvis3 3076ec3 (budget override in kv …` | dropped: not imported; gone with `jarvis` |
| A13.F12 | vm legacyProvider switch path | `jarvis3 chat/src/common/config.ts:104` | dropped: hermit has no provider switch |
| A13.F22 | Doc drift: reset note and reset flag still described | `jarvis3 docs/SANDBOX-VM.md:248 ('the next turn gets the …` | fixed by deletion: `docs/SANDBOX-VM.md` deleted, `docs/CHAT.md` rewritten (17.1) |
| A13.F23 | Doc drift: UI_CUTOVER / edge Worker rollback switches no longer exist | `jarvis3 docs/CLAUDE-CODE.md:196-201` | fixed by deletion: `docs/CLAUDE-CODE.md` deleted (17.1); the yyc-ai skill rewritten at teardown (18.4) |
| A16.F8 | Survival: egress rollout (120 s drain) cutting CONNECT tunnels and … | `chat/src/egress/main.ts:93-105 (announcing=false, delete …` | changed: 3.11.6 drain; conformance check (8.2) and E23; supervisor 19 gains the row (20) |
| A16.F9 | Survival: tools rollout mid tools/call | `chat/src/tools/main.ts:28-37 (drain JARVIS_DRAIN_MS 150 s)` | changed: the `tools` route waits 180 s, above tools' 150 s drain (3.11.4, 3.11.6) |
| A17.F21 | Client replica caches | `paseo/packages/app/src/runtime/replica-cache/index.ts:72- …` | apps unchanged (pigeon 21) |
| A18.F5 | Top folders ws/ and users/ owned by uid 1000 | `storage.rs:144-151` | changed: everything under `ws/` uid 1000 by the PV's `all-squash=1000:1000` (9.6) and hermit's fix-up Job (hermit 15.4) |
| A18.F13 | Client background jobs: session cleanup, slice cleanup, compaction | `.config NoBGJob false, Heartbeat 12 s` | kept on hermit's mount clients (hermit 15.3) |
| A18.F16 | JuiceFS CSI driver HelmRelease | `yyc/deploy/juicefs.yaml:13-65 (HelmRepository …` | moved to `clusters/hetzner/juicefs-csi.yaml`, unchanged (9.1, deploy-workflows 8 H3.a) |
| A18.F17 | Static PV/PVC of the whole volume | `juicefs.yaml:66-106 (PV yyc-ai-jfs 1Pi RWX Retain, …` | changed: PV/PVC `hermit-jfs` (9.6); `yyc-ai-jfs` deleted after teardown (18.4) |
| A18.F18 | Mount pods (one per UI node, shared by UI pods) | `live pod …` | changed: mount pods for `hermit-jfs` on hermit's nodes (hermit 15.2) |
| A18.F19 | Mount-pod restart / upgrade under a running UI pod | `mount pod env JFS_SUPER_COMM + label …` | changed: hermit's `/jfs` mount has `mountPropagation: HostToContainer` (9.6, hermit 15.6) |
| A19.F2 | Snapshot fields read by ai_activity (event diff contract) | `YYC news-stream/monitor.py:1956-2006, 2112-2121` | kept: hermit 18.3 contract 2, golden test, E16 (7.8), cursor carried over (15.5) |
| A19.F3 | Live dashboard panels 9 YYC AI and 10 Systems (/app/live) | `YYC news-stream/web-app/src/routes/-LivePage.tsx:1-21, …` | kept: `live:sys` unchanged because `ai_slim` is unchanged (15.5); fixtures follow (hermit 21.4) |
| A19.F5 | NetworkPolicy web-from-news-monitor | `YYC yyc/deploy/ops.yaml:64-79 (cat -n of …` | changed: `hermit-in` admits `yyc/monitor` on 8080 (10.2); the old policy goes with `legacy/` |
| A19.F7 | Monitor rollout and singleton behaviour (consumer side) | `YYC news-stream/deploy/apps/monitor.yaml:1-9, 58-67, …` | kept unchanged in the news stream |
| A19.F9 | YYC AI tools -> yyc-mcp upstream (service token + X-YYC-User) | `YYC yyc/deploy/config.json:57-65` | changed: tools maps `Aviary-Sandbox-Person` (email) to `X-YYC-User` (17.2 tools item 2) |
| A19.F10 | Mcp-Session-Id handling between tools and yyc-mcp | `news-stream/mcp-edge/src/index.ts:120-130, 154-156` | kept unchanged |
| A19.F11 | Shared rate limit for all YYC AI staff on yyc-mcp | `news-stream/mcp-edge/src/registry.ts:38-47` | kept: one shared 120/min bucket, recorded decision (17.2 tools item 3) |
| A19.F12 | CronJob mcp-cleanup (yyc) | `news-stream/deploy/apps/yyc-mcp.yaml:89-130` | kept: the ConfigMap key `config.json` never disappears, so `mcp-cleanup` is untouched (R40) |
| A19.F13 | yyc-mcp zero-downtime rollout (the dependency YYC AI's tools call) | `news-stream/deploy/apps/yyc-mcp.yaml:1-7, 14-20, 25-29, …` | kept unchanged; its ingress admits `yyc-ai` tools (10.2) |
| A19.F14 | web_fetch own-capture contract (tools -> yyc web) | `YYC news-stream/webfetch.py:1-114` | kept unchanged; `yyc/web` admits `yyc-ai` tools (10.2); capture settings in `tools.json` |
| A19.F21 | CI: ZDR guard parses config.json and greps jarvis3 sources | `YYC .github/workflows/yyc-ai-zdr.yml:1-21` | changed: 13.6 |
| A19.F23 | CI: mcp-image smoke test mounts config.json as the catalogue | `YYC .github/workflows/mcp-image.yml:10-13, 58-69` | changed: mount source `catalogue.json` in the switch commit (R40, hermit 21.5) |
| A19.F25 | Live-dashboard and MCP contract tests in the news stream | `YYC news-stream/tests/test_live_dashboard.py:18-23, 229-360` | changed: fixtures updated in the switch commit (15.5, hermit 21.4) |
| A19.F26 | OpenRouter key yyc-chat usage shown by the monitor | `YYC news-stream/monitor.py:934-962, 2016-2025 …` | kept: the key stays `yyc-chat` (13.6) |
| A20.F15 | ZDR guard (YYC) | `yyc/tests/test_zdr.py:1-77` | changed: 13.6 |
| A20.F16 | iOS simulator smoke (Maestro) | `.github/workflows/yyc-ios.yml:80-88` | kept in claudecodeui (pigeon 21) |

## 23. Review round 2: where each point landed

| # | Point (severity) | Landed |
|---|---|---|
| 1 | No coverage section; 131 egress/tools/ops/apps/drop features have no disposition (major) | Section 22: one row per listed feature (137 after expanding the ranges), generated from the inventory; the A4 rows point to pigeon 21 |
| 2 | Egress drain cuts model streams; supervisor has no "gateway rollout" row; tools call cut at 120 s (major) | 3.11.6 and R38: no new connections, `Connection: close`, streams and tunnels 600 s, fixed names 60 s, tools route 180 s, grace 625 s (9.3, 9.7, 17.2 item 7); conformance 8.2 drain check and E23; supervisor 19 row and the A0.F30 wording required in 20 |
| 3 | `egress.json` lacks self hosts; the VIP mirror breaks (major) | `selfHosts` (VIP + Service names) in 17.2 item 2; decision order fixed names → self hosts → internet in 3.11.1; egress's mirror test (17.2 item 11), the 15.6 item 4 live check, a conformance self-host row (8.2) |
| 4 | Upload cap vs fixed-name routes (minor) | 3.11.4: the cap counts only internet CONNECT and forward bytes; fixed names are limited per request (64 MiB), the model route keeps its `BodyBudget`; conformance row with 40 MiB on one connection |
| 5 | ZDR guard breaks when `config.json` and the remote base go (major) | 13.6: reads `egress.json` (models, `defaultModel`, `smallModel`, `titleModel`; route `model` on openrouter.ai with `model: true`), the jarvis3 commit from the `jarvis3-chat` tag via the GitHub API, the same literals (kept by 17.2 item 9); lands with `egress.json` in P1 and must be green before the switch; checks the legacy stack too until teardown |
| 6 | `openrouter.py` breaks; `smallModel` not allowed; monitor key prefix (major) | 13.6: reads `egress.json` with `smallModel` (plus the legacy list until teardown), guardrail updated and `--verify` run in P1 before the switch; key name `yyc-chat` kept for the monitor's `yyc-` prefix |
| 7 | Catalogue key rename breaks yyc-mcp, mcp-cleanup, the smoke test, docs; timing disagrees (major) | R40, 9.1, 9.2, 18.2: the key stays `config.json` (`files: [config.json=catalogue.json]`), so yyc-mcp and `mcp-cleanup` never change; `mcp-image.yml`'s mount and the listed texts change in the switch commit (hermit 21.5's table); cutover P5.4 changes no reader (20) |
| 8 | No redaction in `aviary-core::log` (minor) | 4.3 and 15.1: `redact()` with today's rules applied by the subscriber to every string field, one unit test per rule, UUIDs, person keys and times kept |
| 9 | Golden test misses `ai_activity`; fixtures use `hub` (minor) | hermit 18.3 contract 2 (already); E16 checks `ai_activity`'s paths and values; 15.5 states what it sees at the switch and the fixture changes in the switch commit |
| 10 | tools owner header unnamed; shared yyc-mcp bucket undecided (minor) | 3.5, 3.11.3: `Aviary-Sandbox-Person` = the lowercased email; 17.2 tools item 2 maps it to `X-YYC-User`; item 3 records "the shared 120/min bucket is kept unchanged"; the supervisor 23 wording is in 20 |
| 11 | No owner of the Paseo timeline rows (blocker) | R27: magpie owns `rows` (dense `rowSeq`, stable epoch, written in the event's transaction); pigeon maps Row → PaseoRow only; magpie 7.3-7.6 already written; cutover 18.2 to point at R27 (20) |
| 12 | Importer packaging conflicts (major) | R15 and 9.12: Deployment `magpie-import` running `magpie import handover` (originally `mirror`; changed by switch-final) from the `magpie` image, one-shots by `exec`, CronJobs for the hermit/egress/tools deltas; PVC `hermit-jfs` read-only; Secret names per 3.7; no separate image or workflow |
| 13 | tofu names disagree (major) | R39 and 14: tunnel `yyc-ai-pigeon` (resource `pigeon`), output `tunnel_token_pigeon`, store key `YYC_CF_PIGEON_TUNNEL_TOKEN`, one path list `staff_ai_paths`, ingress `pigeon.yyc-ai.svc`; cutover and pigeon follow (20) |
| 14 | Migration refusal shape breaks R6; pigeon has no retry (major) | R37 and 3.6: `409 {code: "conversation_migrating", message, retryAfterMs}`; pigeon's 45 s retry and sentences (pigeon 7.10 has them); cutover rewrites its magpie calls in pigeon 14.1's `/v1` camelCase shapes (20) |
| 15 | Gateway owner and routing inconsistent (major) | 3.11: one contract and one owner (platform; YYC = egress for HTTP + hermit's fabric for the network path); one addressing mode, the proxy (R32: no :8081, no `/etc/hosts`); `/v1/sandbox/**` routes, `Aviary-Sandbox-*`, 64 MiB, 30 s long-polls; 3.1, 7.3, 8.2, 9.7, 10.2 changed to match |
| 16 | magpie and hermit ops shapes disagree (minor) | 15.3 is the one shape (stats extended to magpie 16.4's fields; `owner`/`user` are emails; `live` semantics stated); magpie 16.4 and hermit 18.4 already quote it, and hermit's golden test checks it against the monitor contracts |
| 17 | Streaming contract: rows vs events (blocker) | R27 as point 11: rows with dense `rowSeq`, a tool row re-issued on its result, epoch on visible rewrites, M9 `waitMs` over LISTEN, `/v1/changes` with `turn{timelineSeq}`, `sandbox`, `ask`, `ask_resolved`, `timeline_replaced`; harness E21 (reconnect across a compaction) and E22 (zero refetches over interleaved non-row events) |
| 18 | Live frames have no route or consumer (major) | R36: magpie has the receiving side (7.5); this release sends and renders none; 404/409 on `/live` = drop, never retry; E2 asserts whole rows |
| 19 | Single-instance `pg` under every component; async failover loses rows (major) | R31: magpie in its own synchronous 3-instance `magpie-pg` (decision 2), the offset-mismatch repair (magpie 6.12); `pg` stays single-instance, a stated gap (decision 10, 11.5); 19 rows rewritten |
| 20 | Token lookups put hermit on every turn's data path (major) | R33 and 3.11.2: signed tokens checked locally, liveness cached 30 s and served stale on error, never 401/403/407/5xx because of hermit; conformance rows and E24 |
| 21 | Advisory xact lock across long HTTP calls vs `idle_in_transaction_session_timeout` (major) | hermit 10.3 now uses row claims (already); 4.3 makes "no transaction across a network call" a platform rule with a lint, so the 60 s role setting stays a backstop; token lookups no longer use the pool (hermit 3.9) |
| 22 | Watchdog kills healthy turns after a magpie outage (major) | magpie 8.8 measures silence from `availableSince` and asks hermit first (already); 19's rows say so |
| 23 | egress drain 120 s and `/healthz` readiness (minor) | 3.11.6 (600 s; section 24 item 2 says why not 15 min), readiness `/readyz` (9.3, 17.2 item 4), `egress_drain_cut_total` |
| 24 | Two read-path contracts (blocker) | R27 (magpie's rows); from this point: long-poll `waitMs` instead of NDJSON, one change feed `/v1/changes`, the zero-refetch test (E22). The pigeon-projects alternative: section 24 item 1 |
| 25 | Write path on single-instance `pg`; `OFFSET_BEYOND_EOF` stops the agent (major) | as 19: `magpie-pg` (R31); the offset mismatch is a repair from the supervisor's cache (magpie 6.12, R31) |

## 24. Rejected review points

1. **Point 24's recommendation that magpie serve only events and pigeon project them**, folding non-row events into the
   next row's `sourceSeqRanges` or sending seq-carrying no-op rows. Not taken (R27), for the reasons magpie 24 item 1 and
   pigeon 23 item 1 give: the projection is stateful (a tool row changes when its result, a task notification or a sent
   file lands, possibly hours later; a retract must find the rows it touched), so a stateless multi-replica pigeon
   would have to re-read an unbounded window of events per poll; folding into the *next* row stalls the app's dense
   seq gate when no next row comes for hours; Paseo has no no-op item, and the shipped iOS builds cannot be taught one.
   The parts of point 24 that do not depend on this were adopted (23, row 24).
2. **Point 23's "drain for 15 minutes"**, taken as 600 s. Claude Code aborts a model request after its own 600 s
   request timeout, so no stream it waits on can outlive 600 s; a longer bound would only lengthen every egress rollout
   (two replicas: up to 21 minutes already) without saving a stream.
3. **Point 19's "`instances: 3` with synchronous replication for `pg`"**, not done in this refactor. `pg` carries the
   news stream (about 14 GB, growing about 1 GB a day) on a 100 Gi volume with a 6 Gi instance; two more such
   instances need memory the nodes do not have (16.2) and cost money Deyao has not approved. magpie, whose write path
   is the one that must not pause, gets its own small synchronous cluster (R31, decision 2); making `pg` itself highly available was then
   designed and declined for its cost (decisions 8 and 10, 11.5).
4. **Point 15's "one addressing mode: VIP:80 with `/etc/hosts`" as an option**, and the supervisor's proposed row
   "the supervisor calls the fixed names directly and never uses the proxy" (supervisor 23, its R33). The proxy is the
   one mode: Claude Code, pip, npm and curl need it anyway for internet traffic and Claude Code's API and MCP calls
   already travel through it; a second listener (:8081), `Host` routing and `/etc/hosts` entries would be a second
   path to test and secure for no client that needs it. The supervisor's reason for avoiding the proxy (it must not
   depend on `SandboxSelf.env`, the agent's environment) is met without a second mode: it reads the proxy from its
   own boot environment, which hermit sets before the supervisor starts (hermit 5.1, R13), not from `SandboxSelf`.
5. **Point 20's "MAC the token with a key the gateway also holds"**, taken as hermit 6.1 did: an Ed25519 signature with
   public keys published by hermit, so the gateway holds nothing that could mint tokens and needs no new sealed value.
6. **Point 1's count of 131 features**: the ranges it lists expand to 137; section 22 covers all 137.

---

## 25. Two clusters (decision 13)

Text of record: DESIGN 15. This section is the platform's side of it, in the terms of sections 9-16.

### 25.1 What runs where

| Object | Cluster | Applied by |
|---|---|---|
| `yyc/deploy/` (pigeon, magpie, hermit, egress, tools, cloudflared of tunnel `pigeon`, `magpie-import`, `jobs/`, `hermit-gateway`, `hermit-overlay-route`, `bridges.yaml`, `sealed/`) | new | `yyc-ai.yml` |
| `clusters/talos/` (namespaces `yyc-ai`, `yyc-ai-fabric`, `yyc-deploy`, the deployer's RBAC, the fence, the deploy connector, the JuiceFS CSI driver) | new | the new cluster's workflow (Talos session) |
| `clusters/talos/db/` (`magpie-pg` with its backup and `pg-dump-magpie`, `pg-shared`) | new | the same workflow |
| `yyc-ai/legacy/` (namespace `jarvis`, PV `yyc-ai-jfs`, RuntimeClass `gvisor`, `kube-system/sbx-overlay-route`, `catalogue/`, the window's old-side objects) | old | Flux Kustomizations `yyc-ai`, `yyc-ai-secrets` |
| `news-stream/deploy/db/postgres.yaml` (roles and databases `hermit`, `egress`, `tools`), `pg-dump-daily.yaml`, `pg-ingress` | old | Flux `yyc-db` |
| `news-stream/deploy/apps/yyc-mcp.yaml` and the capture route (their bridge NodePorts and ingress rules) | old until T4 | Flux `yyc-apps` |

### 25.2 The `legacy/` move and Flux on the old cluster

WP-39 (DESIGN 15.4): `prune: false` on Kustomizations `yyc-ai` and `yyc-ai-secrets` (`flux-sync.yaml`, applied by
`cluster.sh flux`); a commit adds `yyc-ai/legacy/` as a copy of today's `yyc/deploy/`; a CI step shows the two
renders equal byte for byte after sorting; `flux-sync.yaml`'s `path`s change to `./yyc-ai/legacy` and
`./yyc-ai/legacy/sealed` and the GitRepository's include line to `!/yyc-ai/legacy/`, re-applied; both Kustomizations
Ready at the new revision with no object changed (ReplicaSet hashes and pod ages unchanged, `flux events` shows no
apply other than the unchanged set); a commit deletes the old copy from `yyc/deploy/`; `prune: true` again. From
then on every old-stack change is a Flux commit under `yyc-ai/legacy/`, and the runner waits on the Kustomization
(cutover 11.1).

### 25.3 The bridges (DESIGN 15.5)

Bridges `yyc-bridge-old` (10.0.255.1) and `yyc-bridge-new` (10.0.255.2): Hetzner alias IPs on network `yyc` of one
server, `yyc-bridge1` (cax11 fsn1, Debian, no public IP), running HAProxy in TCP mode; targets by Hetzner server label
(`yyc-cluster=k3s` / `talos`, minus `role=nat`) through their private IPs, connections and health checks from the
bridge's address, TCP health checks (3 s, timeout 2 s, 2 retries); made, checked and removed by
`yyc/infra/bridges.sh` (WP-60; DESIGN 15.5). EUR 7.188 gross a month (EUR 0.01152 an hour; Hetzner API
2026-11-05).

| Link | bridge port → NodePort (`Local`) | Callee policy admits | Caller-side Service (selector-less + EndpointSlice) | Caller egress rule |
|---|---|---|---|---|
| B1 old ui | old :8080 → `jarvis/ui-bridge` 30880 | `jarvis/ui-from-bridge` 8080 from 10.0.255.1/32 | `yyc-ai/ui-legacy` :8080 → 10.0.255.1:8080 | `pigeon-to-ui`, `import-out` |
| B2 pigeon | new :8080 → `yyc-ai/pigeon-bridge` 30890 | `pigeon-in` 8080 from 10.0.255.2/32 | `jarvis/pigeon-bridge` :8080 → 10.0.255.2:8080 | `jarvis/ui-to-pigeon` |
| B3 `pg` | — (`pg-hosts` 30432 on 10.0.0.2, unchanged) | `pg-ingress` 5432 from 10.0.0.0/16 | `db/pg-shared` :5432, NodePort 30433 (`Cluster`) → 10.0.0.2:30432 | `postgres-shared` |
| B4 hermit | new :8083 → `yyc-ai/hermit-bridge` 30893 | `hermit-in` 8080 from 10.0.255.2/32 | `yyc/hermit-bridge` :80 → 10.0.255.2:8083 | the monitor's |
| B5 yyc-mcp | old :8081 → `yyc/yyc-mcp-bridge` 30881 | `yyc-mcp-ingress` 8080 from 10.0.255.1/32 | `yyc/yyc-mcp` :80 → 10.0.255.1:8081 (placeholder) | `tools-out` |
| B6 capture | old :8082 → `yyc/web-bridge` 30882 | the capture ingress from 10.0.255.1/32 | `yyc/web` :80 → 10.0.255.1:8082 (placeholder) | `tools-out` |
| B7 old API | — (10.0.0.3:6443) | k3s `allowed_networks.api` 10.0.0.0/16 | — (a kubeconfig in Secret `yyc-ai/magpie-import`) | `import-out` |

Each bridged request carries its own credential (DESIGN 15.5's table), so the narrow `ipBlock` limits who can reach
the port and does not authorise. The harness (7.7) adds a two-cluster case: a second k3d cluster as the "old" side,
the bridge Services pointing at a test proxy, so E9/E19 fail on a missing bridge rule.

### 25.4 Databases and Secrets

- Connection strings of the new stack name `pg-shared.db.svc.cluster.local:5432` (12.2's `*_DATABASE_URL` values,
  the `hermit-juicefs` `metaurl`) and `magpie-pg-rw.db.svc.cluster.local:5432` (magpie, the importer). 11.4's
  connection budget is unchanged.
- `infra/seal.py`: the cert by folder (`yyc/deploy/sealed/`, `news-stream/deploy/clusters/talos/**` →
  `infra/sealed-secrets-talos.pem`; everything else → `infra/sealed-secrets.pem`), `--check` refuses a mismatch
  (DESIGN 15.9). 12.2's lines for the new stack are sealed with the new cert; `jarvis/ui-env` and `db/pg-role-*` with
  the old.
- 12.2 gains `yyc-ai/magpie-import` key `MAGPIE_IMPORT_LEGACY_KUBE_TOKEN=YYC_OLD_CLUSTER_IMPORT_TOKEN`; 12.3 gains
  `YYC_OLD_CLUSTER_IMPORT_TOKEN` ("token of ServiceAccount jarvis/magpie-import on the old k3s cluster: the importer's
  reads of ui pods across clusters, A1 to A6; sealed into yyc-ai/magpie-import"), deleted in A6.

### 25.5 Placement

Every Deployment, DaemonSet and the `magpie-pg` Cluster in 25.1's new-cluster rows carries a required node affinity
to nodes in network `yyc` (label `yyc.hetzner/network=yyc`, any eu-central location) with a preferred one to fsn1;
egress requires fsn1 (its WARP exit); `magpie-pg` requires the one location chosen for its three instances; and
two-replica Deployments carry a hostname spread with `whenUnsatisfiable: DoNotSchedule` (DESIGN 15.7). `check` (13.3)
asserts all of them. pigeon and hermit, the new-cluster callees of the bridges, carry a 12 s `preStop` sleep (DESIGN
15.5, R6). 16.1's sizes are unchanged except the gateway row: the
`hermit-gateway` pod runs sandboxd itself, 256Mi request = limit (it was a 16Mi sleep loop beside a host unit).

### 25.6 Cloudflare

14.1 unchanged (tunnel `pigeon`'s connectors run on the new cluster); tunnel `yyc-deploy`'s connector runs on the new
cluster (deploy-workflows 12.2); the old tunnel `yyc-ai` keeps its connectors on the old cluster until
`20-old-connectors-0`.

### 25.7 Failure modes across clusters

| Failure | Effect | Handling |
|---|---|---|
| The bridge server yyc-bridge1 down (one server, both bridges) | its links fail: LU (B1) answers 503 to the person's request, the pass-on (B2), the monitor's reads (B4) and tools' yyc-mcp and capture calls (B5, B6) fail; `pg` (B3) and the old API (B7) are unaffected (not bridged) | R6 measured the path; before A3 nothing staff-facing uses B1/B2; inside A3-A5 the runner holds new fences and tells Deyao; `bridges.sh check` runs in `watch`; Hetzner restarts a crashed host's server; `bridges.sh down all && bridges.sh up` rebuilds it in minutes |
| A target node of a bridge reboots | its health check fails within 11 s (3 checks); traffic goes to the other targets with a callee pod | none (R6) |
| The old API (single master) down | B7 reads fail closed: releases wait; LU and ui pods keep serving | wait; N10 keeps the old master untouched until A6 |
| The new API down (all three control nodes) | no `yyc-ai.yml` deploy; the new stack's Pods keep serving; hermit's EndpointSlice watch keeps its last egress targets | the Talos session's runbook |
| `pg-hosts` or `db-worker1` down | as 11.5 (the stated gap), now for the new stack through B3 | as 11.5 |


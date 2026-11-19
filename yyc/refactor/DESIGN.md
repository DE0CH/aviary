# YYC AI refactor: pigeon, magpie, hermit, supervisor (design)

Status: the one design, 2026-11-02, revised the same day for Deyao's decisions (`DECISIONS.md`), last for decisions
10-12 (the shared `pg` stays single-instance; the switch allows brief reconnects and was simplified, section 9), and
revised 2026-11-03 for decision 13 (the new stack lands on the new Talos cluster while the old stack stays on the old
one: section 15, rows D26-D35). Authoritative
input: `/home/claude/artifacts/magpie-target-design.md` (Deyao, 2026-11-02), the decisions of the refactor brief and
`DECISIONS.md` (Deyao, 2026-11-02), which this document does not reopen. It consolidates the final specs and design
notes in `/home/claude/artifacts/refactor/` and is the place to read the system as a whole:

| Spec | Text of record for |
|---|---|
| `spec-platform.md` | conventions every service follows, the reconciliation table R1-R41, the sandbox gateway contract, the aviary repo, CI, harness, YYC deployment, egress and tools changes |
| `spec-magpie.md` | magpie: schema, Claude Code state, events and rows, turns, blobs, shares, REST reference, limits |
| `spec-pigeon.md` | pigeon: sign-in, HTTP surface, the Paseo protocol mapping, live streaming, view-as |
| `spec-hermit.md` | hermit: the contract, the YYC implementation (fabric, scaler, JuiceFS, ops page) |
| `spec-supervisor.md` | supervisor and keeper: process model, FUSE, stream-json driver, lifecycle API |
| `spec-cutover.md` | the importer (sync, byte-exact verification, release transaction), the one-off copies, the front door's tofu, the P0-P2 work, deletion lists; its switch-over sections follow section 9 here |
| `switch-final.md` | the mechanisms section 9 keeps and cites (the legacy upstream's contract, tickets on an outcome, fence and release, upload ingestion, reverse-export completeness); its observer, move rule, person states, remap and in-place upgrade are superseded by section 9 (decision 11). Its section 23 overrides its earlier sections, and "as A §n" imports `switch-A.md` section n |
| `deploy-workflows.md` | how `yyc/deploy` is applied: workflow `yyc-ai.yml`, applier `yyc-apply`, OIDC identity, tunnel `yyc-deploy`, the new cluster as its target with no Flux hand-over (decisions 1 and 13; its 12) |
| `pg-ha.md` | section 4 only: `magpie-pg` (decision 2) and the `db` NetworkPolicies; its plan for an HA Cluster `pg` is dropped (decision 10) |

Precedence: `DECISIONS.md`, then this document (section 9 for the switch, section 15 for the two clusters), then `switch-final.md` for the switch
mechanisms section 9 cites, `deploy-workflows.md` for how YYC deploys and `pg-ha.md` 4 for `magpie-pg`, then platform section 3 (R1-R41), then `spec-cutover.md`, then the component
spec. Section 12 lists every place where a component spec still says something different and
what holds. Feature references `A<area>.F<n>` point into `inventory.json` (706 features in 21 areas; the coverage
tables of the specs assign every one of them a home or a deletion).

Contents
1. Rules and what they mean here
2. Architecture
3. Platform conventions (binding on every service)
4. Every interface in one place
5. Data model
6. Flows: the turn, history, reconnect and the rest
7. Failure handling
8. Zero-downtime operation and version skew
9. The switch-over
10. What is deleted
11. Deployment (YYC)
12. Residual inconsistencies between the specs, decided here
13. Review status
14. Open questions for Deyao
15. Two clusters (decision 13)

---

## 1. Rules and what they mean here

| Rule | How it is applied |
|---|---|
| Services talk only REST; each owns its database behind its API | pigeon has no database; magpie owns `magpie` (Postgres) and bucket `yyc-ai-magpie`; hermit owns `hermit`; egress and tools own `egress` and `tools`. No service reads another's tables. Live delivery is long-poll GETs, never a socket or a LISTEN between services. magpie's LISTEN/NOTIFY is internal to its replicas. |
| No deployment-specific configuration or behaviour switches | pigeon, magpie and the supervisor get no ConfigMap and no env except secrets, bindings (database URL, bucket) and downward-API facts; every limit is a constant in `limits.rs`. Everything that differs between deployments is an *answer* from hermit (`/v1/deployment`, `/v1/people`, `/v1/sandbox/self`) or from `model` (`/v1/aviary/models`). ConfigMaps exist only for the YYC implementations (hermit, egress, tools). CI enforces it at source level (no `std::env::var` outside `aviary-core::env`; no `yyc`/`juicefs`/`openrouter`/`gvisor`/`kube`/`aws-sdk-ec2` outside `crates/hermit`) and at manifest level (`yyc-ai.yml`'s `check` job). |
| Fixed endpoint names | `pigeon`, `magpie`, `hermit`, `model`, `tools`, `egress`: Kubernetes Services on port 80 in the deployment's namespace. Inside a sandbox `magpie`, `hermit`, `model`, `tools` are host names sent to the sandbox proxy (never DNS). |
| Names everywhere, clean cut | crates, images, Deployments, Services, Secrets, metrics, log fields and headers say `pigeon`/`magpie`/`hermit`/`supervisor`; nothing named `ui`, `web`, `jarvis` (as a YYC AI component), `cloudcli` or `JARVIS_*` survives A6. The only `jarvis`/`yyc` strings in pigeon are the shipped apps' frozen wire constants, confined to `crates/pigeon/src/paseo/compat.rs` and renamed in a follow-up protocol version (8.4). |
| Rust | pigeon, magpie, hermit, supervisor, keeper, sandboxd, the importer, the cutover runner, the test fakes. egress and tools stay TypeScript in jarvis3 (they keep their names and role). The Paseo web and iOS apps stay in DE0CH/claudecodeui. |
| Clean-cut migrations | per conversation: owned by the old system, then by magpie, never both (section 9). No dual reads, no mirror after the hand-over, no alias routes. The window code (the importer, pigeon's legacy upstream, tickets and fence, the router's switch module, `migrating`) is deleted in A6; the legacy-path resolution of attachments (`import_uploads`, `uploads.legacy_path`) in A7 (9.7). |
| Zero downtime | every service runs at least 2 replicas, surge-first rolling updates, readiness that never depends on a shared dependency, ordered drains, leader locks for singletons, resumable work (section 8); magpie's own Postgres cluster is highly available (11). Stated gaps: the single gateway node and the single-instance shared Cluster `pg` (7.3, decision 10). The switch-over allows only decision 11's brief reconnects (section 9). |
| GitOps through workflows | a push to DE0CH/YYC applies `yyc/deploy` (the new stack) to the new Talos cluster through workflow `yyc-ai.yml` (decision 1, `deploy-workflows.md`): plan against the live state, deletions only when a commit names them, every rollout waited for. The new cluster's cluster-level objects and databases are applied by that cluster's own workflow. The old stack stays on the old cluster's Flux, from `yyc-ai/legacy/`, until A6 (decision 13, 15.3). |
| Read-only data-analysis product | no confirmations and no outward actions are added anywhere. The agent's AskUserQuestion/ExitPlanMode is relayed only because an unanswerable question would hang the turn. |
| YYC-specific logic only in hermit, egress, tools | the YYC hermit implementation (gVisor on AWS, scaler, packing, migration, JuiceFS, ops page), YYC egress (allowlist, threat DNS, upload cap, OpenRouter with ZDR, budget), YYC tools (yyc-mcp, Exa, TikHub, AnyAPI, capture). |
| Jarvis 4 stays open | not designed here. What is built so another deployment can plug in: the hermit contract with its conformance suite, the gateway contract with its conformance suite, the `model` contract, static binaries in every `aviary-<sha12>` release. |

---

## 2. Architecture

### 2.1 Components

| Component | Repo, language | Replicas (YYC) | State | Job |
|---|---|---|---|---|
| **pigeon** | aviary `crates/pigeon`, Rust | HPA 2-6 | none (memory caches only) | Verifies Cloudflare Access tokens, asks hermit who may sign in and in which role, serves the Paseo web bundle, speaks the Paseo WebSocket protocol on `/paseo-ws` and the apps' small `/api` surface, maps magpie's timeline rows to Paseo rows, streams magpie's feeds to sockets, view-as, forwards `/ops/*` to hermit. |
| **magpie** | aviary `crates/magpie`, Rust | HPA 2-6 | Postgres `magpie` (own CNPG Cluster `magpie-pg`, synchronous), bucket `yyc-ai-magpie` | Owns conversations: list, titles, deletions, shares, the event log (seq per conversation), the timeline rows (row seq per conversation), Claude Code's per-conversation `~/.claude` files, uploads, blobs, files panel, sent-file snapshots, turns (admission, driver, command queue, reports, watchdog), the per-person change feed, view-as audit. |
| **hermit** | aviary `crates/hermit`, Rust (the YYC implementation of the hermit contract) | 2 | Postgres `hermit` | The contract: deployment facts, who may use the deployment, warm hints, sandbox get/start/release/pause/resume/snapshot, the sandbox change feed, workspaces, per-sandbox secrets by key, sandbox tokens. YYC: gVisor sandboxes on AWS burst hosts behind a WireGuard gateway, scaler, packing, live migration, JuiceFS `/workspace` with quotas and clones, ops page and snapshot, admin routes, console tails. Grants no secrets. |
| **supervisor** (+ **keeper**) | aviary `crates/supervisor`, Rust, in the sandbox image | one per sandbox | local run state; all conversation state in magpie | PID 1 is the keeper (tiny, OOM-protected, reaps, owns Claude Code's stdin pipe and the FUSE fd, restarts the supervisor). The supervisor runs Claude Code detached, mounts `~/.claude` through FUSE backed by magpie (a write returns once magpie stored it), executes magpie's command queue, reports turn state, serves hermit's lifecycle API on :7070, reports activity to hermit, materialises uploads, takes sent-file snapshots, makes title calls, guards memory. |
| **sandboxd** | aviary `crates/sandboxd`, Rust (moved from jarvis3) | one per AWS host and on the gateway node | host-local | runsc lifecycle, networking, live migration, image pulls, mTLS API :7443; hermit's only path to a host. |
| **egress** | jarvis3 `chat/src/egress`, TypeScript | 2 | Postgres `egress` | YYC's sandbox gateway (HTTP part): proxy credential check, the fixed names, self hosts (package mirror), internet rules, `model` = OpenRouter with forced ZDR, model list, cost and budget. In-cluster API on 8082 (`model` for pigeon, ops summary for hermit). |
| **tools** | jarvis3 `chat/src/tools`, TypeScript | 2 | Postgres `tools` | Read-only MCP behind `tools` (yyc-mcp, Exa, TikHub, AnyAPI, capture). |
| cloudflared | image | 2 | — | connectors of tunnel `yyc-ai-pigeon` → Service `pigeon`. |
| hermit-gateway | aviary `fabric/host` + `images/sandboxd`, DaemonSet in `yyc-ai-fabric` | 1 node | the host's network namespace (`wg0`, nft, routes); hostPath state | YYC's sandbox gateway (network part): sandboxd in gateway mode as a host-network pod pinned to one Talos node (15.6): WireGuard hub, DNAT of VIP `10.202.1.1:3128` to ready egress pods, overlay route. |
| Paseo web + iOS apps | claudecodeui `paseo/` | — | client caches | unchanged; pigeon speaks their protocol. The web bundle ships as image `pigeon-app`. |

### 2.2 Picture (YYC)

```
 web app / iOS app
     |  HTTPS + WSS  (Cloudflare edge; Access on /api, /paseo-ws, /ops; everything else public)
     v
 tunnel yyc-ai-pigeon -> cloudflared x2 -> Service pigeon:80 -> pigeon x2..6
                                               | REST, one bearer per caller->callee pair
              +--------------------------------+------------------------+--------------------+
              v                                v                        v                    v
       magpie x2..6 (magpie:80)          hermit x2 (hermit:80)    model:80 (egress:8082)   Access JWKS,
        |  ^      |   \                    |    |     |   \                                get-identity
        |  |      |    +-- bucket          |    |     |    +-- EC2, k8s API (read), JuiceFS PVC
        |  |      |        yyc-ai-magpie   |    |     +-- sandboxd mTLS :7443 on hosts
 magpie-pg |      +--> GET hermit          |    +-- supervisor :7070 (control token)
 (3 sync)  |           /v1/sandbox-events  |
           |           PUT/GET .../sandbox  pg: hermit, egress, tools, JuiceFS metadata
           |
           +---- egress x2 (sandbox gateway: proxy :3128) <---- VIP 10.202.1.1:3128 (hermit-gateway DNAT)
                   |  fixed names: magpie, hermit (/v1/sandbox/** only), model, tools (/v1/tools/**)        ^
                   +--> tools x2 --> yyc-mcp, capture, Exa/TikHub/AnyAPI                            |
                   +--> OpenRouter (ZDR forced)                                       WireGuard overlay
                                                                                                     |
                     AWS burst hosts (gVisor): sandbox = keeper (PID 1) -> supervisor -> Claude Code
                                               ~/.claude = FUSE (backed by magpie), /workspace = JuiceFS
```

Everything in this picture runs on the new (Talos) cluster except Cluster `pg` (old cluster until T5, reached through
Service `db/pg-shared`), the AWS hosts and the JuiceFS volume; during the switch the old stack runs on the old cluster
and the two are linked only by the bridges of 15.5.

### 2.3 Endpoint names a sandbox sees

Every client in the sandbox (supervisor, Claude Code, pip, npm, curl) uses the HTTP proxy in its boot environment
(`HTTP_PROXY=http://sbx:<sandbox token>@<proxy>`; YYC `10.202.1.1:3128`). The proxy recognises exactly four host
names, never resolved by DNS:

| Name | Paths a sandbox may use | What it is |
|---|---|---|
| `magpie` | `/v1/sandbox/**` | conversation state (FUSE backing, commands, reports, blobs, sent files) |
| `hermit` | `/v1/sandbox/**` | the sandbox's descriptor, secrets by key, activity reports |
| `model` | `POST /v1/messages`, `POST /v1/messages/count_tokens`, `GET /v1/aviary/models` | the Anthropic Messages API; credentials live behind it |
| `tools` | `GET /v1/tools`, `POST /v1/tools/<name>`, `GET /v1/openapi.json` | read-only REST (decision 15) |
| anything else | — | the internet, as the deployment's gateway allows (YYC: allowlist, threat DNS, public addresses only, 8 MiB upload cap per connection, logged) |

### 2.4 Repositories

| Repo | Holds after the refactor |
|---|---|
| DE0CH/aviary (new, public, AGPL-3.0-or-later, decision 3) | one Cargo workspace: `crates/{aviary-core, aviary-pg, magpie-api, hermit-api, supervisor-api, gateway-api, sandboxd-api, pigeon, magpie, magpie-import (until A6), hermit, supervisor, sandboxd}`, `fabric/{host,aws,gvisor-patches}`, `images/{pigeon,magpie,hermit,sandboxd,sandbox}`, `testing/{fakes, mock-model, fake-tools, test-gateway, fake-sandboxd, fake-access, conformance, e2e, paseo-contract}`, `docs/{CONTRACTS.md, HARNESS.md}`, `.github/workflows/{ci.yml, nightly.yml, clients.yml}` |
| DE0CH/jarvis3 | egress and tools only (image `jarvis3-chat`), with their own databases and the gateway contract; loses `chat/src/web`, `chat/sandbox`, `chat/sandboxd`, `deploy/chat`, the E2B and Kubernetes providers |
| DE0CH/claudecodeui | the Paseo apps; new workflow `pigeon-app.yml` builds the web bundle image; for the window, the router's switch module in `jarvis/router.mjs`, shipped by one ordinary ui rollout in A1 (9.6); loses `jarvis/router.mjs` and the sandbox form of Jarvis mode in A6 (Jarvis 4's local mode stays untouched) |
| DE0CH/YYC | `yyc/deploy` (new layout, section 11), `catalogue.json`, tofu, workflow `yyc-ai.yml` with the applier `tools/yyc-apply`, the `cutover` runner (until A6); the old stack in `yyc-ai/legacy/` (Flux on the old cluster, until A6); the new cluster's cluster-level objects in `news-stream/deploy/clusters/talos/` (`yyc-ai-access.yaml`, `deploy-access.yaml`, `juicefs-csi.yaml`) and `magpie-pg`, `pg-shared` in its `db/`; the roles and databases `hermit`, `egress`, `tools` in `news-stream/deploy/db/postgres.yaml` (Cluster `pg`, old cluster); `yyc/infra/{talos-ready.sh, bridges.sh}` (15.2, 15.5) |

One aviary commit is one release of everything: `ghcr.io/de0ch/<name>:<sha12>` images pushed only after the
integration harness passes on `main`, and one GitHub release `aviary-<sha12>` with every static binary and
`SHA256SUMS`. YYC pins one aviary commit for pigeon, magpie, hermit and sandboxd (checked), and the sandbox image by
digest in `hermit.json` (it changes only with the supervisor or the image, so AMIs are not re-baked per commit).

---

## 3. Platform conventions (binding on every service)

Text of record: platform 3. Summary:

- **Ports.** 8080 API (Service port 80 → 8080), 9090 ops (`/healthz`, `/readyz`, `/metrics`, `/version`; never in
  a Service). Listed exceptions: egress 3128 (sandbox proxy), 8082 (in-cluster API), 9090; supervisor :7070 inside
  the sandbox; sandboxd :7443 mTLS on hosts.
- **Readiness** is 503 only before the process finished its own start (migrations verified at this build's version,
  listener up, magpie's first LISTEN, hermit's config and catalogue loaded, pigeon's deployment facts and JWKS) or
  while draining. It never turns 503 because Postgres, magpie or hermit became unreachable later; requests then fail
  with their own codes. A dependency blip never empties a Service.
- **Drain** (`aviary-core::drain`): preStop sleep, SIGTERM → `/readyz` 503 → ordered hooks (close listeners, end
  long-polls with their "reconnect" answer, hand back leases and leader locks, finish in-flight work up to the
  component's bound) → exit 0 with log `drained`. Grace = preStop + bound + 15 s. A second SIGTERM exits at once
  (work is resumable elsewhere).
- **Paths and versions.** Every service-to-service route under `/v1/`; a breaking change gets `/v2/` beside it and
  `/v1/` is deleted when nothing calls it. Not under `/v1/`: pigeon's public paths (the apps' frozen protocol),
  hermit's `/ops/` pages, `:9090/*`. **Protocol numbers** per surface (`magpie-person`, `magpie-sandbox`,
  `hermit-core`, `hermit-sandbox`, `supervisor-control`, `gateway`), sent as `Aviary-Protocol: <surface>=<n>`; a
  server accepts its current and previous number; `/version` and image labels report provides/accepts so YYC CI can
  refuse an incompatible pin set.
- **Headers** (`Aviary-` prefix only): `Authorization: Bearer <pair token>`, `Aviary-Request` (uuid v7, minted at
  the edge by pigeon, by the gateway for sandbox calls, by hermit for its own work; forwarded unchanged and logged as
  `request`), `Aviary-Protocol`, `Aviary-Person`, `Aviary-Viewer`, `Aviary-View-Session` (pigeon only),
  `Aviary-Admin` (admin tooling), `Aviary-Sandbox-Id`, `-Conversation`, `-Person` (gateway only), `Aviary-Lease`
  (supervisor), `Aviary-Upload-Name`, `Aviary-Upload-Kind` (pigeon). Every receiver strips `Aviary-*` headers its
  caller may not set; a header is trusted only together with the bearer of the caller allowed to set it.
- **JSON**: camelCase bodies and query parameters (serde `rename_all`, a fixture test rejects `_` keys); snake_case
  only inside databases; RFC 3339 UTC with milliseconds; lowercase UUIDs (v7 where ordered); emails trimmed and
  lowercased. Unknown fields ignored both ways; fields are added, never renamed.
- **Errors**: flat `{"code": "<lower_snake_case>", "message": "<English, for logs>", …}`; 400 `bad_request`, 401
  `unauthenticated`, 403 `wrong_caller` or a domain refusal, 404 `not_found`, 409 domain conflicts, 410
  `cursor_expired`, 413 `too_large`, 415 `unsupported_type`, 429 `rate_limited`, 503 `unavailable` (+
  `retry-after`), 507 `quota_exceeded`. Turn error codes inside events (`sandbox.lost`, `run.oom`, …) are data and
  keep their dotted form. `cache-control: no-store` and `x-content-type-options: nosniff` by default.
- **Caller credentials**: one 32-byte random pair token per caller→callee pair; callee env
  `<CALLEE>_ACCEPT_<CALLER>` (comma list, current first, two during rotation; constant-time SHA-256 compare),
  caller env `<CALLER>_TOKEN_<CALLEE>`, store key `YYC_TOKEN_<CALLER>_<CALLEE>` (+ `_NEXT` while rotating). A
  missing or empty accepted list is a start failure. Rotation is three ordinary rollouts. hermit → supervisor uses
  the per-sandbox control token; the sandbox's own calls use the per-sandbox proxy credential; neither is stored.
- **Logs**: JSON lines on stdout, fixed field names, level `info` compiled in; every string passes `log::redact()`;
  identity as `person_key` = `"u"` + first 24 hex of SHA-256(email) (today's `userKey`), never the email, message
  text, file names, tokens or image data.
- **TLS and outbound**: plain HTTP in the cluster (NetworkPolicies decide who connects), TLS at Cloudflare;
  sandboxd keeps mTLS. `reqwest` + rustls + system roots; retries only for idempotent calls or with the caller's
  idempotency key. Postgres `sslmode=require` without CA pinning.
- **Postgres in every service**: `tokio-postgres` + `deadpool` via `aviary-pg`; embedded migrations under an
  advisory lock, recorded with a sha256 (an edited migration is a start failure), **expand-only within a release**,
  contraction in a later release; leader locks via `pg_try_advisory_lock` on a dedicated connection; **no
  transaction is ever held across a network call** (a clippy rule forbids `reqwest` inside `aviary_pg::tx`); long
  work is protected by row claims renewed every 5 s.

Callers and pairs (YYC):

| Caller → callee | Callee accepts | Notes |
|---|---|---|
| pigeon → magpie | `MAGPIE_ACCEPT_PIGEON` | person routes |
| pigeon → hermit | `HERMIT_ACCEPT_PIGEON` | deployment, people, warm, `/ops/*` |
| pigeon → model (egress 8082) | `EGRESS_ACCEPT_PIGEON` | models list |
| magpie → hermit | `HERMIT_ACCEPT_MAGPIE` | sandboxes, feed, workspaces, people |
| hermit → magpie | `MAGPIE_ACCEPT_HERMIT` | `/v1/hermit/**`, `/v1/ops/**` |
| hermit → egress | `EGRESS_ACCEPT_HERMIT` | ops summary |
| gateway → magpie / hermit / tools | `MAGPIE_ACCEPT_GATEWAY`, `HERMIT_ACCEPT_GATEWAY`, `TOOLS_ACCEPT_GATEWAY` | on behalf of a verified sandbox |
| tools → hermit | `HERMIT_ACCEPT_TOOLS` | sandbox liveness and state |
| monitor → hermit | `HERMIT_ACCEPT_MONITOR` | `/ops/snapshot` |
| admin → hermit / magpie / egress | `*_ACCEPT_ADMIN` | operator tooling, audited |

---

## 4. Every interface in one place

Shapes are in the contract crates (`*-api`), whose golden fixtures both server and client tests round-trip. The
tables name each route, its caller, and its purpose; the spec column gives the text of record.

### 4.1 Contract crates and protocol surfaces

| Crate | Surfaces (protocol numbers) | Owns the types of | Implemented by | Used by |
|---|---|---|---|---|
| `magpie-api` | `magpie-person` 1, `magpie-sandbox` 1 | every magpie request, response, event kind, Row/RowItem, change, command, report | magpie, `aviary-fakes` fake-magpie | pigeon, supervisor, hermit, importer, ops tools |
| `hermit-api` | `hermit-core` 1, `hermit-sandbox` 1 | Deployment, Person, Sandbox, SandboxEvent, ReleaseReason, Workspace, `SandboxSelf`, `ActivityReport`, `ActivityReply`, token key and liveness, the sandbox token format | hermit (YYC), fake-hermit, any other implementation (Jarvis 4, open) | pigeon, magpie, supervisor, gateway, tools |
| `supervisor-api` | `supervisor-control` 1 | the :7070 surface, the local socket (`/run/supervisor/agent.sock`) protocol, re-exports hermit's sandbox types | supervisor, fake-supervisor | hermit |
| `gateway-api` | `gateway` 1 | proxy credential format, the four fixed names and their routes, `Aviary-Sandbox-*`, models route (R18), token lookup types | egress (YYC), test-gateway (reference) | everything in a sandbox, conformance |
| `sandboxd-api` | — | sandboxd's REST types | sandboxd, fake-sandboxd | hermit |

pigeon serves only the app protocol and has no contract crate; its contract with the apps is the Paseo protocol
checked by `testing/paseo-contract` against every supported client build (`clients.txt`).

### 4.2 Apps → pigeon (public, frozen by shipped builds; pigeon 5, 7)

HTTP (signed-in prefixes are exactly `/api`, `/paseo-ws`, `/ops`):

| Route | Identity | Upstream |
|---|---|---|
| `GET /paseo-ws` (upgrade) | person or view-as | the Paseo socket (below) |
| `GET /api/jarvis/signin?next=`, `GET /api/jarvis/app-signin?state=` | person | sign-in redirects (web, iOS app scheme from hermit) |
| `GET /api/jarvis/me` | person or view-as | identity, role from hermit H2, first name from Access get-identity |
| `POST /api/jarvis/view-as` | admin | mints an HMAC view-as token; magpie M17 records it |
| `POST /api/jarvis/warm` | person | hermit H3 (coalesced per person+conversation per 30 s) |
| `GET /api/providers/claude/models` | person | `model` `GET /v1/aviary/models` |
| `POST /api/assets/files` | person | magpie M13 (streamed) |
| `GET /api/jarvis/sessions/{id}/files/raw?path&inline&sent` | person | magpie M14 |
| `GET /api/blobs/{blobId}?v=&sig=` | person + pigeon's signature | magpie M15 |
| `GET, POST /api/jarvis/sessions/{id}/shares`; `DELETE /api/jarvis/shares/{id}`; `GET /api/jarvis/shares/{id}`, `…/files/raw`; `POST …/continue` | person (writes refused in view-as) | magpie M16 |
| `GET /ops`, `/ops/*` | admin | hermit `/ops/*` |
| static files, the app shell | none / optional | the `pigeon-app` bundle (hashed chunks of the last builds kept 30 days) |

App-facing errors on `/api/*` keep the apps' frozen shape `{"success": false, "error": {"code", "message"}}`
(`NOT_SIGNED_IN`, `NOT_ALLOWED`, `VIEW_AS_REFUSED`, `VIEW_AS_READ_ONLY`, `UNAVAILABLE`, `NOT_AVAILABLE`).

Paseo socket (`/paseo-ws`): hello/server_info handshake with `protocolVersion` (1 today; 2 after the rename,
8.4). Client messages, by group, and the magpie call each makes:

| Group | Client messages | magpie / hermit |
|---|---|---|
| static and developer-machine messages | heartbeats, labels, hosts, usage, plugins, icons, provider features, daemon config, checkout status … | none: constant replies (pigeon 7.5.1) |
| conversation list | `fetch_workspaces`/`fetch_agents` (subscribe), `project.list` | M1 snapshot + M12 change feed |
| timeline | `fetch_agent_timeline_request {direction: tail\|before\|after, cursor: {epoch, seq}}`, `agent.timeline.set_subscription` | M9 |
| send | `send_agent_message_request {clientMessageId, text, images, files}` | M7 (until A6, a legacy-owned conversation's send goes through LU with a ticket, 9.1) |
| new conversation | create with `idempotencyKey`, `creation.subscribe.request` | M3, M4 |
| stop | stop request | M8 |
| rename, delete, delete for good, restore | | M5, M6 |
| the agent's questions | permission response for AskUserQuestion / ExitPlanMode | M11 |
| everything else (terminals, git, files outside the panel) | | refused as unsupported (pigeon 7.5.10) |

Server pushes: `agent_stream` timeline rows (seq-gated by the app: `seq == endSeq + 1`), `agent_stream` turn
lifecycle (`turn_started`, `turn_completed`, `turn_failed`, `turn_canceled`, delivered after the rows up to the
turn's `timelineSeq`), `agent.timeline.replacement` (epoch change), `workspace_update`/`agent_update` (list),
`yyc.sandbox_status` (preparing/ready), permission requests (asks). Localised sentences (zh-HK, en) for every error
code are pigeon's (pigeon 7.9).

### 4.3 pigeon → magpie (person surface; magpie 16.1; `Aviary-Person` on all but M15, M17)

| # | Route | Purpose |
|---|---|---|
| M1 | `GET /v1/conversations?state=active\|deleted&limit&cursor&search` | list snapshot `{conversations, nextCursor, list: {generation, headSeq}}` |
| M2 | `GET /v1/conversations/{id}` | one Conversation |
| M3 | `POST /v1/conversations` `{idempotencyKey, message}` | create with first message; 201 `{conversation, turn}` |
| M4 | `GET /v1/creations/{idempotencyKey}` | re-attach an interrupted create |
| M5 | `PATCH /v1/conversations/{id}` `{title}` | rename |
| M6 | `POST …/{id}/delete`, `POST …/{id}/restore`, `DELETE …/{id}` | Recently deleted, restore, delete for good |
| M7 | `POST /v1/conversations/{id}/turns` `{clientMessageId, text, images, files}` | admit a turn; 202 `{turn}` (200 repeat); 409 `turn_running` / `conversation_deleted` |
| M8 | `POST /v1/conversations/{id}/cancel` | stop the active turn |
| M9 | `GET /v1/conversations/{id}/timeline?tail=1\|before=S\|after=S&limit&waitMs&liveAfter` | rows `{epoch, minSeq, maxSeq, hasOlder, hasNewer, rows, live}`; `waitMs` ≤ 25 000 long-polls `after` |
| — | `GET /v1/conversations/{id}/live` | live state of the active turn (receiving side built; unused in this release, 12 D3) |
| M10 | `GET /v1/conversations/{id}/prompts` | the prompts index |
| — | `GET /v1/conversations/{id}/search?q` | search inside a conversation |
| M11 | `POST /v1/conversations/{id}/asks/{askId}/answer` | answer an AskUserQuestion / ExitPlanMode |
| M12 | `GET /v1/changes?after=<int\|latest>&limit&waitMs` | the person's change feed: `conversation_upserted`, `conversation_removed`, `turn{phase, timelineSeq}`, `sandbox`, `ask`, `ask_resolved`, `timeline_replaced` |
| M13 | `POST /v1/uploads` (raw body, `Aviary-Upload-Name`, `-Kind`) | store an upload; 201 `{uploadId, path, …}` |
| M14 | `GET /v1/conversations/{id}/files/raw?path&sent&inline`, `GET …/files` | files panel and downloads (live workspace via hermit, or a sent-file snapshot) |
| M15 | `GET /v1/blobs/{blobId}?variant=thumb\|display\|full` | blob bytes (pigeon signed the capability) |
| M16 | `GET, POST /v1/conversations/{id}/shares`, `DELETE /v1/shares/{id}`, `GET /v1/shares/{id}`, `GET /v1/shares/{id}/files/raw`, `POST /v1/shares/{id}/continue` | shares |
| M17 | `POST /v1/view-as` `{admin, person, session, expiresAt}` | record a view-as session before any use |

### 4.4 pigeon → hermit, model, Cloudflare (pigeon 14.2-14.4)

| # | Route | Purpose |
|---|---|---|
| H1 | `GET http://hermit/v1/deployment` | `{product: {name, locale}, signIn: {access: [{teamDomain, audience}], appScheme}}`; no capability flags |
| H2 | `GET http://hermit/v1/people/{email}` | `{email, allowed, role: admin\|staff\|null}`; re-checked so a removal closes sockets within 75 s |
| H3 | `POST http://hermit/v1/warm` `{person, conversationId}` | a demand hint; never creates or extends a sandbox |
| H4 | `GET\|HEAD http://hermit/ops/{rest}` | operator page and snapshot (hermit re-checks admin) |
| — | `GET http://model/v1/aviary/models` | `{default, small, title, contextTokens, models: [{id, label, reasoning}]}` |
| — | `https://<team>/cdn-cgi/access/certs`, `…/get-identity` | JWKS, first name |

### 4.5 magpie ↔ hermit (hermit 3.6-3.8; magpie 8.3, 8.7, 9.7, 16.3-16.4)

magpie → hermit:

| Route | Purpose |
|---|---|
| `PUT /v1/conversations/{cid}/sandbox` `{person}` | get or start (idempotent, p99 < 50 ms; 200 ready, 202 starting; 403 `not_allowed`; 409 `person_mismatch`; 503 `sandbox_busy {reason, retryAfterS}`) |
| `GET /v1/conversations/{cid}/sandbox?waitMs=&afterVersion=` | long-poll until the version changes (ready/gone); used by the driver and by the lease check |
| `POST /v1/conversations/{cid}/sandbox/release` `{reason: deleted\|offboarded\|requested}` | from magpie's outbox on delete, purge |
| `POST …/sandbox/pause\|resume\|snapshot` | contract calls; YYC answers 501 `not_supported`; nothing in the product uses them |
| `GET /v1/sandbox-events?after=<seq>\|latest&limit&waitMs` | the only path from hermit to magpie for sandbox state; magpie's job leader follows it, cursor in its own DB (R8) |
| `PUT\|GET\|DELETE /v1/workspaces/{wid}`, `GET …/entries?path`, `GET …/files?path` (Range), `POST …/clone {from}` | workspaces: live reads for the files panel, share freeze and continue (clone), revoke and purge (delete) |
| `GET /v1/people`, `GET /v1/people/{email}` | catalogue reads (who may own conversations) |

hermit → magpie:

| Route | Purpose |
|---|---|
| `POST /v1/hermit/persons/{person}/purge` `{reason: offboarded}` | the operator's person purge (409 `person_allowed` while still allowed) |
| `POST /v1/hermit/workspaces/exists` `{ids}` | which workspace ids still belong to a conversation or share (orphan cleanup) |
| `GET /v1/ops/people?tz`, `GET /v1/ops/view-as?limit&before`, `GET /v1/ops/stats` | the ops page's figures (no message contents) |

Workspace id convention (owned by magpie): a conversation's workspace id = its conversation id; a share's frozen
workspace = `uuidv5(6f1c9d2e-0b7a-4f53-9a43-5b2f3f1e8c61, "share:" + shareId)`.

### 4.6 supervisor → magpie, through the gateway (magpie 16.2; `Aviary-Lease` on state changes)

| Route | Purpose |
|---|---|
| `POST /v1/sandbox/lease` | single-writer lease `{leaseId, epoch, conversationId, providerSessionId, mainTranscript, resync}`; granted only to hermit's current sandbox (409 `not_current_sandbox`, `conversation_migrating`) |
| `GET /v1/sandbox/cc/tree`, `GET /v1/sandbox/cc/file?path&offset&length` | read `~/.claude` (conversation scope) |
| `POST /v1/sandbox/cc/write?path&offset&mtimeNs&opId&create&mode&turnId` (≤ 16 MiB body), `…/truncate`, `…/rename`, `…/unlink`, `…/mkdir`, `…/rmdir`, `…/setattr` | the FUSE backing; idempotent by `opId`; appends carry the expected offset (409 `offset_mismatch`, `offset_beyond_eof`, `lease_lost`) |
| `GET /v1/sandbox/commands?after&waitMs`, `POST /v1/sandbox/commands/{cmdSeq}/ack` | the command queue: `start_turn`, `interrupt`, `kill`, `title`, `answer_ask`, `stop_tasks` |
| `POST /v1/sandbox/turns` `{turnId, origin: "agent"}` | an agent-started turn (background task follow-up) |
| `POST /v1/sandbox/turns/{turnId}/state` | `running`/`completed`/`failed`/`canceled` with providerSessionId, transcriptPath, tasks, lastUuid, error code |
| `POST /v1/sandbox/turns/{turnId}/live` | live frames (not sent in this release) |
| `POST /v1/sandbox/tasks`, `POST /v1/sandbox/asks`, `POST /v1/sandbox/asks/{askId}/resolved` | background tasks, asks |
| `POST /v1/sandbox/heartbeat` | every 15 s; the watchdog's evidence |
| `GET\|PUT /v1/sandbox/blobs/{sha}` | upload originals and model copies (get), sent-file bytes (put) |
| `POST /v1/sandbox/sent-files`, `POST /v1/sandbox/sent-files/{toolUseId}/commit` | sent-file snapshot (two phases) |

### 4.7 supervisor → hermit, model, tools, through the gateway

| Route | Purpose |
|---|---|
| `GET http://hermit/v1/sandbox/self` (ETag) | `SandboxSelf`: ids, control token hashes, managed CLAUDE.md, title instructions, env for the agent's processes, resources, lifecycle timers, secret keys |
| `GET http://hermit/v1/sandbox/secrets`, `…/secrets/{key}` | secrets by key (YYC: `[]`, every key 404 `not_granted`) |
| `POST http://hermit/v1/sandbox/activity` | `ActivityReport` every 15 s and on state change (`running`/`stopping`/`held`/`idle`, tasks, memory, memory-kill events) → `ActivityReply {state, release}` |
| `POST http://model/v1/messages` (Claude Code via `ANTHROPIC_BASE_URL=http://model`), `GET http://model/v1/aviary/models` | the model; title calls |
| `POST http://tools/v1/tools/<name>` | the tools REST API, called with `curl` as the managed skills say (decision 15) |

### 4.8 hermit → supervisor (:7070, control token; supervisor 15.4)

| Route | Purpose |
|---|---|
| `GET /v1/health` | `{ok, ready, protocol, version, bootId, busy, state}`; `ready: true` = descriptor read, lease held, FUSE mounted, `/workspace` present |
| `POST /v1/lifecycle/release` `{reason: ReleaseReason, code, force, deadlineMs}` | 200 `{released: true}` after Claude Code stopped and every accepted FUSE write is stored; 409 `sandbox_busy` when unforced and busy |
| `POST /v1/lifecycle/prepare` `{kind: pause\|snapshot\|migrate, force, deadlineMs}`, `POST /v1/lifecycle/resumed` | `snapshot` = `sync(2)` before a workspace clone; `pause`/`resumed` for pause-capable implementations (not YYC); `migrate` answers at once |
| `GET /v1/state` | operator detail |
| `POST /v1/exec` | uid 1000 only, behind the audited admin route; no product path uses it |

### 4.9 The sandbox gateway contract (platform 3.11, `gateway-api`)

- **Addressing**: one mode, the proxy. Absolute-form `http://<name>/…` or `CONNECT host:443`. The proxy's own
  addresses (`selfHosts`) serve implementation-local paths (YYC: the pypi/npm mirror).
- **Order per request**: credential → fixed name → self host → internet rules. A fixed name never reaches the
  internet checks.
- **Credential**: `Proxy-Authorization: Basic sbx:<token>`. The token `hst1.<kid>.<payload>.<sig>` is Ed25519-signed
  by hermit and checked locally against `GET http://hermit/v1/sandbox-token-keys` (refreshed every 5 min, at most
  every 10 s on an unknown kid, last set kept on failure), with expiry and source address. Liveness from `GET
  http://hermit/v1/sandbox-tokens/{sandboxId}` cached 30 s and served stale without limit while hermit fails.
  **A hermit problem never turns a valid token into 401/403/407/5xx.** Revocation takes effect within 30 s.
- **Routes**: the four of 2.3; the gateway strips the client's `Authorization`, `Proxy-Authorization`, `x-api-key`
  and every `Aviary-*`, adds `Aviary-Sandbox-Id/-Conversation/-Person`, `Aviary-Request` and the callee's bearer;
  body limits 64 MiB per request for magpie, hermit, model; tools' own 2 MB.
- **Transport**: streamed both ways (first byte ≤ 200 ms after the upstream's), long-polls up to 30 s, idle 120 s,
  tools ≥ 180 s, model unbounded (Claude Code's 600 s bounds it); the upload cap counts only internet traffic; fixed
  names `magpie`/`hermit` are hot-path (no per-request log row, p99 ≤ 5 ms added).
- **Drain**: refuse new connections, `Connection: close` on kept-alive ones, model streams and tunnels up to 600 s,
  fixed-name requests up to 60 s, tools up to 180 s; grace 625 s.
- **In-cluster API** (not for sandboxes): `GET /v1/aviary/models` (pigeon), `GET /v1/ops/summary` (hermit),
  `GET /v1/admin/egress-log` (admin).
- tools → hermit: `GET /v1/sandbox-tokens/{sandboxId}` (refuses sandboxes not `starting`/`ready`); owner =
  `Aviary-Sandbox-Person`, forwarded to yyc-mcp as `X-YYC-User`.

### 4.10 Inside the sandbox (local, supervisor 4, 13)

| Interface | Between | Purpose |
|---|---|---|
| keeper control socket `/run/supervisor/keeper.sock` (SEQPACKET) | supervisor ↔ keeper | `SPAWN`, `STDIN` (msg_id, written at most once), `CLOSE_STDIN`, `SIGNAL`, `PUT_FD`/`GET_FD` (the FUSE fd survives supervisor restarts), `EXITS`, `STATUS` |
| `/dev/fuse` | sentry ↔ supervisor | `~/.claude` (FUSE 7.31 offered by the fork, reply minor ≥ 28 for 1 MiB writes; no mmap), request journal for restart safety |
| stream-json on Claude Code's stdin/stdout | supervisor ↔ Claude Code | user messages, control requests (`can_use_tool`, interrupt), result and task messages |
| `/run/supervisor/agent.sock` | `supervisor hook send-user-file`, `sandbox-secret` ↔ supervisor | sent-file snapshots (fd passing), secrets by key |
| `/run/supervisor/docker.sock` | docker wrapper ↔ supervisor | dockerd on demand (through the keeper) |
| boot environment | hermit implementation → keeper | `HTTP(S)_PROXY` with the sandbox token, `NO_PROXY`, `HOME`; nothing else meant for the supervisor |

### 4.11 hermit → the YYC fabric and stores (hermit 13-15)

sandboxd REST over mTLS :7443 (`sandboxd-api`: create, delete, list/alive, spec, checkpoint/restore for migration),
EC2 (launch, terminate, reconcile by tag `app=hermit-host`), the Kubernetes API (read: Deployments, Pods,
EndpointSlices for the egress DNAT targets), JuiceFS (CSI mount at `/jfs` via PVC `hermit-jfs`, `juicefs` CLI for
quotas and clones), Postgres `hermit`, the catalogue file, egress `GET /v1/ops/summary`.

### 4.12 Operator and monitor surfaces

hermit `/ops/` (HTML, admins through pigeon), `/ops/snapshot?tz` (JSON; the news-stream monitor's `ai_slim` and
`ai_activity` contracts), `/v1/admin/**` (sandboxes enabled, release, recycle, exec, person purge, controls; every
call audited in `admin_events`); magpie `/v1/admin/conversations/**` (content reads, `Aviary-Admin` required,
audited); egress `/v1/admin/egress-log`. Console tails: hermit keeps each released sandbox's last 64 KiB of console
log. Tracing one conversation: a script greps `kubectl logs` of every service by conversation id and `request`.

---

## 5. Data model

### 5.1 Identifiers and sequences

| Thing | Id | Sequence |
|---|---|---|
| person | trimmed, lowercased email; `person_key` for logs | change feed `seq` per person (gaps allowed), `change_generation` |
| conversation | uuid (imported ones keep CloudCLI's session id) | event `seq` contiguous from 1; row seq (`rowSeq`) dense from 1; `epoch` from 1, +1 only when a retract removes or rebuilds a visible row; `cmd_seq` for commands |
| turn | uuid (v7; import turns uuidv5) | — |
| sandbox | uuid, a new one per sandbox of a conversation | `version` per row; hermit's feed `seq` (bigserial, committed in order) |
| workspace | = conversation id; share's = uuidv5(share) | — |
| blob | sha256 of the bytes | — |
| share | 22-char base64url of 16 random bytes (today's format) | — |
| upload | uuid; opaque path `uploads/<uploadId>/<name>` until attached | — |
| cc op | `opId` uuid (derived per FUSE request) | — |
| lease | uuid with `lease_epoch` | — |

The target design's "events by seq" are realised as two magpie sequences kept in the same transaction: the
**events** (the immutable record, every kind) and the **timeline rows** (the protocol-neutral projection of what a
conversation shows). pigeon streams and pages rows, not events, because the app accepts a live row only when its seq
is the previous plus 1 and a stateless multi-replica pigeon cannot pair tool results with calls or fold task
notifications (R27). Events stay the record for admin reads and import verification.

### 5.2 magpie (database `magpie`, own Cluster `magpie-pg`; magpie 4)

| Group | Tables | Notes |
|---|---|---|
| people | `persons` | created on first request; change feed head and generation |
| conversations | `conversations` | title, `name_source`, `deleted_at`, `head_seq`, `row_head`, `epoch`, `cmd_head`, `run_state` (`idle`/`admitted`/`preparing`/`running`/`stopping`/`held`), `active_turn`, `last_turn`, `background`, `sandbox_phase`, `provider_session_id`, `main_transcript` (a path), `cc_bytes` (quota 2 GiB), `cc_resync`, lease columns, hermit's view (`hermit_sandbox`, `hermit_state`, `hermit_version`), `migrating` (dropped in A6), `legacy_fence_at` (A6), the model stored at release, `origin` |
| feeds | `changes` (per person, 30 days), `events` (immutable; `kind`, `turn_id`, `data`, `meta`, `text`, `is_prompt`, `viewer`), `rows` (`row_id` = first rowSeq, `seq_end`, kind `user`/`assistant`/`reasoning`/`tool`/`error`, `item`, `source_seqs`, `tool_use_id`, `message_id`, `block_index`), `row_seqs` (every rowSeq → its row, makes `after` paging dense), `tool_links`, `live_frames` (unlogged, ephemeral) | event kinds: `conversation.created`, `user.sent`, `turn.started`, `sandbox.preparing`, `sandbox.ready`, `turn.stop_requested`, `turn.ended`, `title.changed`, `files.sent`, `task.status`, `ask.pending`, `ask.resolved`, `retract`, `cc.user`, `cc.assistant` |
| turns | `turns` (origin `person`/`agent`/`import`, state, seqs, timestamps, error, usage, sandbox, driver claim, deadline), `commands`, `report_ids` | |
| Claude Code state | `cc_files` (kind `lines`/`bytes`/`dir`), `cc_lines` (stub + spans of extracted blob content), `cc_limbo` (truncated/moved lines awaiting reuse), `cc_pages` (256 KiB pages for byte files), `cc_ops` (idempotency, 24 h) | the whole `~/.claude` scope is per conversation, auto-memory (`projects/-workspace/memory/`) included (decision 6); only the main transcript path produces events; compaction rename-over, tombstone rewrites and set-asides are matched line by sha256 so nothing is re-emitted; a truncate's removed lines become `retract` after 10 s of quiet |
| blobs | `blobs`, `blob_staging` (bytes staged in Postgres on the agent's hot path, uploaded by a job), `blob_refs` (owner = conversation \| share \| upload) | bucket layout `blobs/<2 hex>/<sha>`, `thumbs/<sha>-512.webp`, `display/<sha>-2000.webp`; media ≥ 1 KiB base64 and text ≥ 16 KiB (not assistant prose) are extracted; rows carry references and previews (4 000 chars) |
| uploads, files | `uploads`, `sent_staging`, `sent_files` | |
| shares | `shares`, `share_rows` | frozen rows + a frozen cc scope (row copies) + a workspace clone |
| audit | `view_as_log` | |
| jobs | `outbox` (calls to hermit that must happen once), `hermit_feed` (cursor), `availability` (magpie's own availability ticks for the watchdog) | |
| import and window (until A6, the legacy-path set until A7) | `import_sources`, `import_shares`, `import_control`; `legacy_tickets`, `conversations.legacy_fence_at`; legacy-path set: `import_uploads`, `uploads.legacy_path` | cutover 7.3; switch-final 4.3, 23 C5, C10 (as reduced by 9) |

Background jobs (one job leader by advisory lock unless marked): purge (30 days), outbox, hermit feed follower,
blob GC (24 h grace) and weekly bucket reconcile, retention; on every replica with `skip locked`: blob uploader,
availability tick, limbo flush, live-frame cleanup, turn sweeper.

### 5.3 hermit (database `hermit` on Cluster `pg`; hermit 10)

`hosts`, `sandboxes` (one live per conversation by unique index; state, phase, version, timers `last_active_at`,
`reserved_until`, `expires_at`, `hard_expires_at`, busy, boot id, supervisor version, migration columns, claims),
`sandbox_events` (the feed, 7 days), `sandbox_starts`, `host_events`, `sandbox_logs` (console tails), `demand_hints`,
`controls` (operator switches), `admin_events` (kept for good). Leadership `(7300, <objid>)` advisory locks; long
work (starts, migrations, releases) are resumable tasks under row claims renewed every 5 s; janitor every 30 s; scaler
every 5 s; LISTEN/NOTIFY internal for long-poll wake-ups.

### 5.4 egress, tools (databases `egress`, `tools` on Cluster `pg`)

egress: `egress_log` (today's columns + `person`, `request`; no retention while the budget's `since` predates its
oldest row), `budget_state`. tools: `tool_calls`. Each runs its own migrations under its own lock.

Cluster `pg` is on the old cluster until T5 (after A6); hermit, egress and tools reach it from the new cluster through
Service `db/pg-shared` (15.5 B3, 15.8). `magpie-pg` is on the new cluster.

### 5.5 Outside the databases

- JuiceFS volume `yyc-ai` (metadata `jfs_yyc_ai` on Cluster `pg`): only `ws/<workspace id>` after P6, uid/gid 1000,
  quota 1 GiB and 1 000 inodes per workspace (hermit 15).
- Supervisor local state (dies with the sandbox by design): `/var/lib/supervisor/state/run.json` (run state, command
  cursor, pending inputs), the write-through content cache (256 MiB), the request journal, the node table; local
  scope of `~/.claude` (sessions registry, shell snapshots, backups, settings).
- pigeon: nothing durable (sockets, pollers and caches in memory).

---

## 6. Flows

### 6.1 A turn

```
app ──send──> pigeon ──M7──> magpie ──PUT/GET sandbox──> hermit ──sandboxd──> host: keeper → supervisor
                                 ^  │                                                      │
                                 │  └── commands (long-poll) ◄────────────────────────────┤
                                 └──── cc writes, reports (through the gateway) ◄──────────┘ Claude Code → model, tools
pigeon ◄──M9 after=<rowSeq> (long-poll), M12 changes── magpie
```

1. **Send.** The app sends `send_agent_message_request {clientMessageId, text, images, files}` on its socket (any
   pigeon replica). pigeon checks identity (cached), refuses writes in view-as, calls magpie M7. During the switch
   window a conversation the old stack still owns is written through pigeon's legacy upstream under a ticket instead
   (9.1); the person sees no difference.
2. **Admission** (one magpie transaction): lock the conversation; idempotency by `clientMessageId`; refuse
   `turn_running` unless `run_state` is `idle` or `held`; attach the person's uploads; append `user.sent` and
   `turn.started` (the `user.sent` event makes a `user` row at the next rowSeq); insert the turn (`admitted`) and
   command `start_turn` (plus `title` while the conversation is untitled and has had fewer than 3 attempts); changes
   `turn{started}`, `conversation_upserted`; NOTIFY. 202. The user's text is stored before anything can fail.
3. **Rows reach the tabs.** NOTIFY wakes every held M9 long-poll on every magpie replica; each pigeon replica's
   `TimelinePoller(person, conversation)` receives the user row and pushes one `agent_stream` frame (serialised once
   per locale) to every subscribed socket; the `ChangePoller(person)` pushes `turn_started` and the list upsert.
4. **Sandbox.** The admitting magpie replica claims the turn as driver (30 s claim, renewed every 10 s; any replica
   takes over a lapsed claim) and calls hermit `PUT /v1/conversations/{id}/sandbox`. 200 ready → turn `starting`.
   202 starting → long-poll `GET …?waitMs=25000&afterVersion=v` until `ready` or `gone`. Not ready within 1 s →
   `sandbox.preparing` event, change `sandbox{preparing}` → the app's 「準備中」 chip (`yyc.sandbox_status`).
5. **hermit starts it** (a resumable task any replica can run): capacity (≤ 10 live; a pool victim is soft-released
   if needed) → workspace (ensure `ws/<id>` with its quota) → placing (packing on an AWS host) → host (scaler
   launches one if needed) → image (digest) → creating (sandboxd `POST /v1/sandboxes` with the boot environment
   carrying the signed sandbox token in the proxy URL) → supervisor (poll :7070 `/v1/health` until `ready: true`) →
   `ready`; each change is a feed event and wakes the held long-poll.
6. **Supervisor boot**: keeper (PID 1) → supervisor → `GET hermit/v1/sandbox/self` → :7070 up (not ready) → lease
   from magpie (granted only to hermit's current sandbox for the conversation) → load the cc tree, reconcile the
   cache → mount FUSE, hand the fd to the keeper → prefetch the main transcript and pre-spawn
   `claude --resume <providerSessionId>` → start the command poll, heartbeat, activity loop, memory guard → health
   `ready: true`.
7. **The turn starts.** The command poll returns `start_turn` (created at admission, so it is already waiting). The
   supervisor fetches upload blobs and writes files into `/workspace/uploads`, builds the user message (model copies
   of images as image blocks), writes it to Claude Code's stdin through the keeper (msg_id: at most once even across
   a supervisor crash), acks the command, reports `running` with `providerSessionId` and `transcriptPath` (which sets
   `main_transcript`; lines written before that are promoted in the same transaction).
8. **The agent works.** Claude Code calls `http://model/v1/messages` through the proxy (egress verifies the token
   locally, injects OpenRouter's key, forces ZDR, enforces the model shortlist and the budget, logs cost) and
   `http://tools/v1/tools/<name>` (decision 15). Bash and other tools reach the internet only through egress's rules.
9. **Every transcript write is stored before it returns.** Claude Code appends a line → the sentry sends a FUSE WRITE
   (≤ 1 MiB each, serialised per file) → the supervisor journals it and calls `POST magpie/v1/sandbox/cc/write` with
   `opId`, expected offset and lease → magpie's one transaction (row lock, lease and `opId` check, quota, blob
   extraction with bytes staged in Postgres, `cc_lines`, classification into `cc.assistant`/`cc.user` events, the
   rows they make or update at the next rowSeq, `tool_links`, changes, NOTIFY) commits on a synchronous standby →
   200 → the supervisor updates its write-through cache and replies → `write()` returns. Target p50 ≤ 15 ms for a
   10 KiB append, p99 ≤ 150 ms for a 1 MiB image line at magpie's handler. Rows stream to the tabs as in step 3.
   A tool row is re-issued at a new rowSeq when its result, status, task or sent files arrive.
10. **End.** Claude Code's `result` arrives on stream-json. The supervisor waits until every transcript write of the
    turn is acknowledged, then reports `completed` (or `failed` with `run.exited`/`run.error`/`run.oom`/
    `cc.write_stuck`) with `lastUuid`; magpie answers 409 `transcript_pending` until the transcript holds that uuid.
    magpie appends `turn.ended`, sets `run_state` `idle` (or `held` while background tasks run), emits change
    `turn{completed, timelineSeq}`; pigeon delivers `turn_completed` to a socket only after that socket has the rows
    up to `timelineSeq` (or after 2 s).
11. **Activity and idle.** The supervisor reports activity to hermit every 15 s (`running`/`stopping`/`held`/
    `idle`). After 30 min idle (and the 120 s reservation of the last `PUT` passed) hermit soft-releases: the
    supervisor agrees only when idle, stops Claude Code, waits for every accepted write, answers 200; hermit destroys
    the sandbox; the feed's `gone` clears magpie's lease. The next turn gets a new sandbox and `claude --resume` reads
    the transcript from magpie through FUSE. `/workspace` persists on JuiceFS.

Agent-started turns: when a background task's notification makes Claude Code continue by itself, the supervisor
posts `POST /v1/sandbox/turns {origin: "agent"}`; magpie records an agent turn (`held` → `running`). A person's send
while `held` is admitted and supersedes the background work.

Titles: the `title` command carries the first user prompt; the supervisor calls `model` with hermit's
`title.system` and the `title` model from `/v1/aviary/models`, acks with the title; magpie appends `title.changed`
(80 characters max; a provisional title of 24 characters is shown meanwhile; 3 attempts).

### 6.2 History

Opening a conversation: `fetch_agent_timeline_request {direction: tail}` → M9 `tail=1&limit=40` → the last 40 rows
by `seqStart`, with `epoch`, `hasOlder`. Scrolling up: `before=<oldest seqStart>&limit=40`. Each page is one indexed
range read on `(conversation_id, row_id)`; cost is proportional to the page, not to the conversation (harness E6
asserts the time per page stays within 2× of the first over 120 turns). Rows carry images as blob ids; pigeon signs
`/api/blobs/{id}?v=thumb|display&sig=` URLs; a tap loads `full`. Large tool output carries a 4 000-character preview
and a blob id for the full text. The prompts index (M10) and search inside a conversation are magpie reads.

### 6.3 Reconnect and resume by seq

A socket drops (network, pigeon drain, pigeon crash). The app reconnects with backoff (1.5 s doubling to 30 s) to
any replica, re-sends hello and its subscriptions, gets a fresh list snapshot (with `headSeq`; the change poller
resumes after it), and for each open conversation asks `after={epoch, seq: endSeq}`. magpie's epoch is a column,
identical on every replica and across restarts, so the answer is exactly the missed rows; a running turn continues
where it stopped, nothing lost or doubled. Turn state comes back in the conversation snapshot. Sends are idempotent
by `clientMessageId`, creates by `idempotencyKey` (M4 re-attaches an interrupted create). An epoch change (a
retract that removed or rebuilt a visible row) is announced by `timeline_replaced` and makes pigeon send
`agent.timeline.replacement` and restart from the tail. The turn itself never depends on any socket or any pigeon
replica.

### 6.4 The other flows

| Flow | Path |
|---|---|
| New conversation | M3 creates the conversation (`conversation.created`) and admits the first turn in one call; then 6.1 |
| Stop | M8 → `turn.stop_requested`, command `interrupt` → supervisor interrupts Claude Code (≤ 15 s) → reports `canceled`; no confirmation in 30 s → `canceled` with `stop.forced` + command `kill`. Stop before delivery cancels the queued `start_turn` at once. Stop while preparing only stops waiting (the sandbox keeps starting and is used by the next send or idles out). |
| Agent's question | Claude Code's `can_use_tool` for AskUserQuestion/ExitPlanMode → supervisor `POST /v1/sandbox/asks` → `ask.pending`, change `ask` → pigeon pushes the permission request → the app answers → M11 → command `answer_ask` → supervisor answers the control request → `asks/{id}/resolved` → `ask.resolved`. A run that waits only on an open ask is `asking`, not busy (DECISIONS 17): hermit's idle release (or any release) parks the turn (`parked`, magpie 8.1a; no ask resolved, the question stays on screen), and the answer resumes it in a new sandbox (`start_turn` with `answers[]`, delivered as the turn's next message, supervisor 7.6). The wait counts in no turn timer; `ASK_TIMEOUT` is a 7-day safety limit. |
| Upload and attachments | app `POST /api/assets/files` → pigeon streams to M13 → magpie stores the blob (bucket, off the hot path), makes the model copy of images (long edge ≤ 2 000 px, ≤ 3 932 160 bytes, EXIF-rotated), returns `uploadId` and an opaque path; M7 attaches it (`uploads/<name>` unique in the conversation); the supervisor writes it into `/workspace/uploads` from the blob. Limits: 50 MiB per file, 10 images + 10 files per turn. |
| Sent files (SendUserFile) | Claude Code's hook runs `supervisor hook send-user-file`, which passes open fds over `agent.sock`; the supervisor `PUT`s the bytes as blobs, `POST /v1/sandbox/sent-files` (phase 1), then `…/commit` → `files.sent` event → the tool row's `sentFiles`; downloads by M14 `sent=<toolUseId>` stay available after the workspace file changes or is deleted. |
| Files panel | M14 → magpie → hermit `GET /v1/workspaces/{conversationId}/entries|files?path` (live read, `openat2` beneath the root) |
| Share | M16 create: hermit clones the workspace to the share's id (metadata-only `juicefs clone`, after asking a ready supervisor to `prepare snapshot` = `sync(2)`), then one magpie transaction freezes the rows up to the last finished turn, a cc-scope copy and blob refs. View: any signed-in person, frozen rows only, files from the frozen workspace. Continue: clone the share workspace to a new conversation id, copy events, rows and cc scope; the new conversation is the viewer's; the first turn gets a new sandbox and resumes the copied transcript. Revoke: delete rows, scope, refs, queue `DELETE` of the share workspace. A share of a conversation in Recently deleted answers 404 until restored. |
| Delete, restore, delete for good | M6 delete sets `deleted_at` and queues `release_sandbox` (outbox); restore clears it (a new sandbox on the next turn); delete for good (or the 30-day purge) removes the conversation's rows and refs and queues `DELETE /v1/workspaces/{id}` plus each live share's workspace; blob GC frees unreferenced bytes after 24 h. |
| Offboarding | removing someone from `catalogue.json` blocks them within about 2 min (kubelet sync, hermit's 30 s re-read, pigeon's re-checks closing sockets within 75 s; gateway liveness turns `live: false`; hermit force-releases their sandboxes). Nothing is deleted until an operator runs `POST /v1/admin/people/{email}/purge` (hermit → magpie purge); no automatic deletion after any time (decision 7). |
| View as | an admin `POST /api/jarvis/view-as` → HMAC token, M17 records it; every magpie call carries `Aviary-Viewer` and `Aviary-View-Session`; writes are refused (`VIEW_AS_READ_ONLY`) by pigeon on HTTP and socket; reads as the person; the ops page lists sessions (M17, `/v1/ops/view-as`). |
| Warm | pigeon sends hermit a hint when a person opens the app or a conversation; the scaler counts hints younger than 120 s; a hint never creates a sandbox. |
| Ops page | admin's browser → pigeon `/ops/*` → hermit `/ops/` (sandboxes, hosts, starts, releases, budget from egress's summary, people figures from magpie's ops reads, verdict); the monitor reads `/ops/snapshot` with its own token. |

---

## 7. Failure handling

### 7.1 Principles

- Every write that crosses a service boundary is idempotent (`clientMessageId`, `idempotencyKey`, `opId`, report
  ids, command `cmdSeq`, hermit's per-conversation `PUT`), so every retry is safe.
- Work outlives the process that started it: magpie's driver claims and sweeper, hermit's resumable start, migrate and
  release tasks, the supervisor's journal and keeper. Nothing that matters lives only in memory except sockets.
- A dependency failure is answered, not hidden: 503 `unavailable` with `retry-after`, never a readiness flip.
- The supervisor never drops a report and never acknowledges a write magpie did not store. Under general
  unavailability it blocks and retries for as long as it takes; only one operation stuck for 10 min while heartbeats
  succeed becomes EIO and `cc.write_stuck` (R26).
- Turn failures end as an `error` row with a code; pigeon renders a localised sentence; the person can send again.

### 7.2 Matrix

| Failure | Effect | Handling |
|---|---|---|
| pigeon replica dies or drains | its sockets close | apps reconnect to another replica and resume by seq (6.3); turns unaffected |
| magpie replica dies | held long-polls end; driver claims lapse | pigeon and the supervisor retry on another replica; the sweeper takes over turns after the 30 s claim; job leader lock moves |
| magpie Postgres primary fails | writes stall during switchover | synchronous standby (no acknowledged write lost; `failoverQuorum` never promotes a stale standby, so a double failure waits for a manual promotion while magpie answers `unavailable`); FUSE writes block and retry; watchdog counts only time magpie could accept heartbeats. If a write were lost anyway (`offset_mismatch` or `resync` on the lease), the supervisor re-sends the missing suffix from its write-through cache; only a real divergence stops the agent with EIO |
| Bucket unreachable | blob uploads fail | hot-path blobs stay staged in Postgres (reads served from staging); above 2 GiB staged, cc writes that add new blobs answer 503 until it drains; uploads and sent files answer 503 |
| hermit down (all replicas) | no new sandboxes; feed paused | running turns continue (gateway serves cached liveness without limit; supervisor's hermit calls retry); magpie's driver retries until the 15 min admission deadline (`sandbox.timeout`); pigeon serves cached deployment facts and people answers |
| hermit replica dies mid-start or mid-migration | — | the task's claim lapses and another replica resumes it from its recorded step |
| Sandbox host lost | its sandboxes die | host heartbeats every 5 s; hermit marks them `gone`/`sandbox_lost` within about 10 s; magpie ends the turn `sandbox.lost` (harness E11: within 40 s); the next turn starts a new sandbox and resumes |
| Sandbox vanished (OOM of the whole sandbox, runsc died) | — | `vanished` → `gone`/`sandbox_lost`, as above |
| Supervisor crash | — | the keeper restarts it (0.1 s backoff), it re-reads `run.json`, takes the FUSE fd back, replays its journal, re-attaches to the running Claude Code, re-polls unacked commands; Claude Code never notices (harness E10) |
| Claude Code exits mid-turn | turn fails `run.exited` | the next turn starts a new generation with `--resume` |
| Memory pressure in the sandbox | — | the user-space memory guard kills the largest tool process (keeper and supervisor are `oom_score_adj -1000` and mlocked); `memory_kill` reported to hermit (ops page); a turn killed by it is `run.oom` |
| One cc write keeps failing | that file stalls | after 10 min with heartbeats succeeding: EIO, `cc.write_stuck`, generation ended, `cc_resync` set; the next lease resyncs |
| Two sandboxes for one conversation (failed migration bookkeeping, host judged lost but still running) | — | magpie grants the lease only to hermit's current sandbox; the other stays unready polling every 30 s, never writes; hermit destroys it |
| Gateway (egress) replica drains | — | 3.11.6 drain; a cut model stream is retried by Claude Code, a cut fixed-name call by the supervisor with the same `opId` |
| Gateway node (WireGuard hub) down | every sandbox loses its network path | stated gap (7.3): turns stall and retry, nothing is lost; hermit judges nothing while the gateway is unreachable |
| tools or an upstream down | tool calls fail | the model sees a tool error; turns go on |
| Model budget exhausted | egress answers 402 | Claude Code writes `API Error: 402 …`; magpie tags it `model.budget`; pigeon shows the budget sentence |
| No capacity (pool full, hosts full, quota) | turn waits, then fails | `sandbox_busy` → `sandbox.busy` with retry time, never a 500 (harness E17) |
| Catalogue edit invalid | — | hermit keeps the last good catalogue, logs and shows a `problem` finding; CI refuses it first (`hermit catalogue-check`) |
| JuiceFS metadata stall | `/workspace` I/O blocks | turns stall; `~/.claude` is unaffected (magpie); hermit's workspace calls answer 503 `workspace_unavailable` |
| Cluster `pg` restart (single instance, decision 10) | `pg-rw` and `pg-hosts` unavailable for the restart (tens of seconds; longer if `db-worker1` is lost) | stated gap (7.3): hermit answers `unavailable` (503 + `retry-after`) and serves token keys, liveness, people and deployment facts from memory, so running turns go on; egress serves its last budget status and buffers log rows; new sandboxes wait; JuiceFS metadata stalls, so `/workspace` I/O blocks until it is back; magpie (own cluster) and `~/.claude` are unaffected; no acknowledged write is lost (the volume survives a restart; a lost node is a restore from `pg-pitr`) |
| Change feed cursor older than 30 days | 410 | pigeon restarts at the head and closes that person's sockets (1012) to re-snapshot |
| Socket too slow | queue full | closed with 1013, the app reconnects and refetches; never slows others |

### 7.3 Stated gaps

1. **The single gateway node** (hermit 14.7): the WireGuard hub, the VIP DNAT and the overlay route run on one node
   (a host-network pod pinned to one Talos node, 15.6); its reboot is not zero-downtime for running turns. Mitigation:
   the Talos upgrade workflow upgrades that node only through a scripted maintenance runbook (disable starts, wait for
   no busy sandbox, do the work, re-enable; 15.2 N8), and a restart of the gateway pod itself leaves the data plane in
   place (15.6). Two gateway nodes are a separate change.

2. **The single-instance shared Cluster `pg`** (decision 10): `hermit`, `egress`, `tools`, the JuiceFS metadata and
   the news stream share one Postgres instance on `yyc-pool-db-worker1` (old cluster until T5; 15.8). A restart (operator or minor upgrade, the
   node's reboot) pauses new sandbox starts and `/workspace` I/O for its duration; running turns' model calls and
   `~/.claude` (magpie's own synchronous `magpie-pg`) go on. Mitigation: such restarts are planned outside working
   hours and never between A1 and A5; every client already reconnects (7.2). Making `pg` highly available was
   designed (`pg-ha.md`) and declined for its EUR 98.90 a month.

The switch-over adds no stated gap beyond decision 11's allowed reconnects (9).

---

## 8. Zero-downtime operation and version skew

### 8.1 Rollouts (platform 9.3)

All Deployments: `RollingUpdate maxSurge 1, maxUnavailable 0`, `minReadySeconds 5`, PDB `minAvailable 1`, spread by
hostname with `matchLabelKeys: [pod-template-hash]`, priority class `serving`, readiness on `:9090/readyz`.

| Component | Replicas | preStop | Drain | Grace | Singletons |
|---|---|---|---|---|---|
| pigeon | HPA 2-6 | 5 s | close sockets with jitter over 15 s (apps resume by seq); finish uploads/downloads ≤ 300 s | 320 s | none |
| magpie | HPA 2-6 | 5 s | answer held long-polls at once, release driver claims and the job leader lock, finish writes ≤ 30 s | 75 s | job leader (advisory lock); driver claims |
| hermit | 2 | 10 s | hand back claims and leader lock; a migration's final phase ≤ 60 s | 90 s | leader (janitor, scaler, reconcile) |
| egress | 2 | 10 s | 3.11.6 (600 s model streams, 60 s fixed names, 180 s tools) | 625 s | — |
| tools | 2 | 10 s | 150 s | 180 s | — |
| herald | 2 | 10 s | finish posts and file uploads to Telegram ≤ 310 s (an upload may take 300 s) | 330 s | — |
| cloudflared | 2 | — | `--grace-period 3m` | 200 s | — |

Rollouts are applied by `yyc-ai.yml`, which waits for every rollout before it prunes anything (11). pigeon's drain
closes request-free sockets first; during A3-A4 the runner holds routine pigeon rollouts (9.3).

Harness E9 restarts pigeon, magpie, hermit and the gateway in turn during a 90 s turn and asserts the turn completes,
every socket saw every row once, and no request got anything but success or `unavailable`+`retry-after`. E20 and
E23 check each drain. P4 includes one planned rolling update of every component during working hours with a turn
running.

Schema changes are expand-only within a release (old and new replicas run together); contractions ship in a later
release once no running version reads the object (A6's drop of the window code and A7's of the legacy-path set are pigeon first, then magpie).

### 8.2 Between the services

Protocol numbers per surface; each server accepts its current and previous number. The supervisor's number is fixed
for a sandbox's life (it never upgrades in place), so magpie and hermit keep accepting the previous `*-sandbox`
number until every sandbox on the old image is gone (at most hermit's 8 h + 1 h, or `POST /v1/admin/recycle`).
YYC's CI refuses a pin set whose image labels do not fit together.

### 8.3 Apps

- Web tabs: the `pigeon-app` image keeps the hashed chunks of the last 5 builds and of 30 days, so a tab loaded from
  build N keeps fetching its lazy chunks from build N+1; the first bundle is seeded with the chunks of every `ui`
  image served within the 720 h Access session lifetime, and their commits are in `clients.txt` (9.6).
  pigeon and the bundle are pinned together.
- iOS builds: the Paseo contract suite validates every frame pigeon sends against the schema of every client build in
  `clients.txt` (each TestFlight upload appends its commit); the switch needs no app release.

### 8.4 The wire-constant rename (follow-up, not part of the switch)

The apps' `/api/jarvis/*`, `jarvis_view_as`, `#jarvis-view-as=`, `<meta name="yyc-signed-out">` and
`yyc.sandbox_status` move to `aviary` names in a claudecodeui change that sends `protocolVersion: 2`. One pigeon
release serves protocol 1 and 2 side by side (two protocol versions, one name each); protocol 1 is deleted when
`pigeon_hello_total{protocol="1"}` has been 0 for 30 days.

---

## 9. The switch-over

Text of record: this section (revised 2026-11-02 for decisions 10-12). `switch-final.md` still specifies the
mechanisms this section keeps and cites by number (the legacy upstream's contract, tickets closing on an outcome, the
fence and release, upload ingestion, reverse-export completeness); everything it adds to avoid reconnects (the router
observer and its move rule, person states, proxy sockets, the seq remap, the 72 h tab wait, the live patch or
in-place router upgrade) is superseded by this section, which wins wherever the two differ. `spec-cutover.md` keeps
what neither changes: the importer's sync code and byte-exact verification, the one-off copies of hermit, egress and
tools, the tofu of the front door, the P0-P2 work and the deletion lists.

What the switch may and may not do (decision 11, which relaxes decision 4): **allowed** are one ordinary ui rollout
before the switch (pushed at a moment with no running work on the old pods, 9.6; every open tab reconnects once), moving every open old tab right away at T0 (a ~2 s
"正在重新連線…" notice, then the tab resumes), one timeline repaint of a shown conversation when it changes owner, and
one UI refresh on a rollback. **Not allowed**: a read-only conversation, a cut or lost running turn, lost input, a
failed attachment, a lost ask. Decision 9: the switch runs whenever it is ready; no time window (working hours allowed, never required; Deyao 2026-11-06).
Decision 12: no early hand-over of an admin (no R5).

### 9.1 Shape

Two complete stacks with no shared component, on two clusters (decision 13, section 15): the old one (ui, web, its
egress, tools, connectors, fabric) in namespace `jarvis` on the old k3s cluster, the new one in `yyc-ai` on the new
Talos cluster with its own egress, tools, connectors (tunnel `pigeon`) and hermit fabric (10.202/15, EC2 tag
`app=hermit-host`). Every link below that crosses clusters goes through a bridge of 15.5. The window is short: from T0 a few minutes until every open tab is on pigeon
and the old connectors are at 0, then every idle conversation is handed over in one pass, and the rest follow as their
old turn ends and their old sandbox idles out (at most about 30 min after the last old turn). The stacks are linked
only by:

| Link | Direction | What it carries |
|---|---|---|
| the importer (`magpie import handover`) | old → new | bulk import (A2), pre-verification, the per-conversation fence and release, upload ingestion, the running mirror (old running state of legacy conversations into magpie's `run_state`), warm hints, the egress spend copy |
| pigeon's legacy upstream (LU, `crates/pigeon/src/legacy/`) | pigeon → the old ui :8080 through bridge B1 (`ui-legacy:8080`, 15.5), as the person, with the newest Access token pigeon verified for them | everything a tab does with a conversation the old stack still owns: its timeline and live stream from the old child, and its writes (send, stop, rename, delete, restore, warm, share create and revoke, the old copy of an attached file), writes under a ticket |
| the router's switch module (claudecodeui `jarvis/router.mjs`, shipped by the one ordinary ui rollout of A1, 9.6) | importer (through the old API's pod proxy, B7) and runner (the session's admin path) → old router; the router's pass-on → pigeon through B2 | per-person counts of first-hop sockets that are not LU (with their outstanding request ids, last inbound frame time, `opaque` flag) and of in-flight non-LU HTTP requests; `move` (close each such socket at a request-free, quiet instant); after the first move, new non-LU upgrades refused before the 101 and non-LU HTTP passed on to pigeon; `restart` of an idle person's child (rollback only, 9.10); the old purge switched off |
| the DNS record of `ai.yyc-internal.com` | — | which tunnel new connections reach |
| JuiceFS | shared, mounted from both clusters (metadata in `pg`, through B3 for the new side) | each `ws/<id>` used by one stack at a time (only the owning stack can start its sandbox) |
| `egress import-jarvis` (every minute, idempotent by source id) | old → new | the new egress counts the old stack's spend; the old egress does not see new-stack spend for the few hours of the window (preflight headroom, 9.8) |

### 9.2 Ownership

```
 conversation:  [legacy] ──fence──► [fenced] ──release txn──► [owned]          writer: old stack via LU | nobody (≤ 3 s) | magpie
                    ▲ unfence (a fingerprint changed) ┘
                [owned] ──reverse fence + export──► [legacy]                     (rollback only, 9.10)
 gate:          set once: flip settled, every ui pod reports 0 non-LU sockets, old connectors at 0
```

- There are no per-person states. Before the gate every conversation is legacy and reachable two ways (an old-path
  socket that has not been moved yet, or LU); after the gate the old path no longer exists (the old connectors are at
  0), so the old stack can be written only through LU, which pigeon routes by magpie's ownership row. That is what
  makes a release safe without any refusal inside the old router.
- A conversation magpie has no row for while its person has legacy ones is legacy too: one made on the old path since
  the importer's last pass (rehearsal R1, 2026-11-06). pigeon serves it through LU (a tab that subscribed before gets
  one replacement in the old child's epoch); a write to it waits up to 30 s for the import, since its ticket needs the
  row; from the import it is an ordinary legacy conversation (pigeon README "Ownership").
- The gate is one row (`import_control.handover_allowed_at`), set by the runner when (a) the flip is live from an HK
  vantage and the settle time has passed, (b) every ui pod, enumerated from the old cluster's Kubernetes API (B7) with Terminating pods
  included, has reported zero first-hop non-LU sockets (opaque ones included) and zero in-flight non-LU HTTP requests
  in reports generated after the last move, for 60 s, and (c)
  Deployment `jarvis/cloudflared` has 0 replicas and no pod. A per-person gate exists for rehearsal R1 only (bot):
  (b) restricted to that person, (c) not required.
- Every input that gates a release fails closed (a missing or stale ui report, an unanswered running-set read, an
  unknown ticket state): it only delays a release, never changes what a tab shows.
- Invariants, each a test: one writer per conversation at every instant; no write is ever refused because of the
  switch (a legacy conversation's writes go through LU, a fenced one's wait at most 3 s); every running old turn ends
  on the old stack; every user row arrives once, in order, dense (fence-race fuzz); no socket is closed by the switch
  with a request in flight, as far as the server can know it (9.3: no outstanding request id, ≥ 5 s without an inbound
  frame, the person not typing or attaching; the residual is measured, 9.9); every file path a composer holds resolves on the path that receives the send; the old
  stack retires only with zero legacy conversations and zero LU sockets.

### 9.3 Tabs

- **At T0 the DNS moves** (the switch commit); new connections land on pigeon, established sockets stay on the old
  tunnel until moved. Rehearsal R0 checks on the test host that the record change itself does not close established
  sockets (20 sockets, 10 min); if it does, the ingress-rule variant (tunnel `yyc-ai`'s rule, whose connectors run on
  the old cluster, pointed at `http://pigeon-bridge.jarvis.svc.cluster.local:8080`, bridge B2) is measured the same way, and if neither holds, A3 waits for Deyao (a close that is not
  request-aware could fail an in-flight send).
- **The move, right after the settle time.** Every 10 s the runner reads pigeon's per-person activity
  (`GET /internal/switch/activity`: the last warm call and the last upload pigeon received for each person; after T0
  every HTTP call of an old tab already reaches pigeon) and calls `POST /__jarvis/move?person=<p>` on every ui pod for
  each person with neither in the last 60 s, i.e. who has typed nothing for 30 s (the composer sends a warm call on
  the first keystroke and then at most every 30 s, `paseo/packages/app/src/yyc/warm.ts`) and is not attaching. The
  router closes a non-LU first-hop socket of `p` only at an instant when (1) it has no outstanding request id, (2) no
  inbound frame other than `client_heartbeat` or a ping has arrived on it for ≥ 5 s (switch-final F2), and (3) the
  move call is less than 10 s old; all three are re-checked at the close. It sends a close frame (1012) and keeps
  reading the TCP socket for 2 s: a request frame read after the close decision is counted in
  `switch_inflight_closes_total` and not forwarded (the client has already rejected it). An `opaque` socket (9.6) is
  never closed by the move. Each moved tab shows its ~2 s reconnect notice and reconnects through the DNS to pigeon.
  No visibility, device or heartbeat rule: phones, WebKit and visible tabs are moved alike; a backgrounded iOS app finds
  its socket gone and reconnects on its next foreground, as after any ui deploy.
- **After the first move no new old-path request can start.** From the runner's first `move` call (without
  `person`, which only arms this) the router answers every new non-LU upgrade with HTTP 503 before the 101 (no request
  can be in flight on a socket that never opened; the app retries through the edge, which reaches pigeon) and passes
  every non-LU HTTP request on to pigeon on the new cluster through bridge B2 (`http://pigeon-bridge:8080`, 15.5)
  unchanged, Access headers included (pigeon verifies them as at the edge; NetworkPolicy `jarvis/ui-to-pigeon`), so a stale edge location that still routes to tunnel `yyc-ai`
  changes nothing for the person. `POST /__jarvis/admit` turns both off (a rollback before the gate, 9.10).
- **The runner keeps moving until the gate is set.** When every pod has reported zero non-LU sockets and zero
  in-flight non-LU HTTP for 60 s it pushes `old-connectors-0`, and it keeps calling `move` while the old cloudflared
  pods terminate (their 200 s grace lets a request already inside the old tunnel finish) and until the gate is set.
  A socket that stays busy, opaque or with an active person for 10 min is reported to Deyao with its person; it keeps
  the gate unset and nothing else waits on it (bounds in 9.7). The residual (a Send pressed within the network delay
  of a close, after ≥ 5 s of silence on the socket and 30 s without a keystroke) is counted and fuzzed (9.9), and
  stated in 14.
- **Cursors.** A moved tab reconnects holding the old child's epoch `E`. Until its release a conversation is served
  through LU by the same old child (the person's process is untouched by the move), so the cursors stay valid and
  nothing repaints. At the release pigeon sends one `agent.timeline.replacement` in magpie's epoch on every socket
  subscribed to the conversation, the frame every ui deploy produces today when a child restarts: the tab refetches
  the tail; the composer (draft text and attached files) is not touched (a persona-rig assertion, 9.9). No seq map,
  anchors, captured old timeline or "not shown" rule exist.
- **Lists** are native from T0: magpie's imported rows, with each legacy conversation's running state written by the
  importer's running mirror (every 5 s, from the running sets of 9.4) and titles and `updatedAt` by its catch-up
  (every 5 s). Creates from T0 are native (born owned). A conversation created on pigeon in the minutes before a
  person's old tab is moved appears in that tab when it reconnects.
- **pigeon rollouts and scale-in.** A pigeon rollout is an ordinary reconnect (LU reopens with the person's newest
  token; an LU close never stops an old turn). From T0 to the end of A4 every pigeon rollout is held and the HPA is
  pinned (`minReplicas = maxReplicas`, set in the switch commit and reset in `30-ui-0`, switch-final C1.4 restored), so
  nothing but the move reconnects a tab and LU is reopened only behind a reconnect that came through the edge, which
  enforces the Access session itself; a pigeon fix inside that span is an ordinary reconnect and needs Deyao's OK.
  pigeon's drain closes request-free sockets first (kept after A6: it protects in-flight input in every rollout).

### 9.4 The per-conversation hand-over

Conversation `c` is released when all hold (switch-final 8.1 and C5, minus the person and presence conditions):
1. the gate is set;
2. `c` is in no old child's running list (`holds.js`; an answered parked question of the busy route's `asking` list,
   whose turn has yet to run, counts as running) in two consecutive reads that started after its last ticket closed
   on an **outcome** (turn started and processing, or completed/failed), never on frame delivery;
3. ≥ 60 s quiet: no ticket, and the old child's pending user messages and preparing state for `c` are empty (read
   through LU);
4. no `jarvis.sandboxes` row of `c` is not `gone` (the importer waits for the old janitor's own 30 min idle release
   and never releases an old sandbox; the host's JuiceFS client has then flushed and unmounted);
5. `c` is pre-verified byte for byte and every fingerprint still matches: cc files, sessions row, sent files, failed
   turns, upload references, share rows.

Then: fence transaction (no open ticket) → re-stat of every fingerprint → a read of its parked questions
(`jarvis_open_questions`; an answered one unfences: its turn has yet to run on the old stack) → release transaction
(one more pass over the person's `cloudcli-assets/`, the title mapped to a `name_source` that blocks retitling, the old
child's current model stored on the conversation, every unanswered parked question carried over as a native open
question of a parked turn keyed by its `tool_use_id`, magpie 8.1a) → M12 + NOTIFY. Fence to release p99 ≤ 300 ms, max 3 s; a changed fingerprint unfences
and `c` stays legacy. A send arriving during the fence waits in the ticket call and becomes the first native turn. Before
the gate a warm call is a ticketed write like any other; from the gate a warm call for a legacy conversation goes to
hermit only (a warm hint, no ticket, no LU call; switch-final C15), so typing in a legacy conversation does not keep
resetting its 60 s quiet period. The importer sends hermit warm hints every 60 s while any conversation
is legacy, so the first native turn after the mass release finds warm hosts.

Expected shape: at the gate every idle conversation releases in the first pass (seconds); a conversation with a
running turn, a hold or a warm old sandbox releases after it ends and idles out (≤ 30 min after its last old turn).
Real bounds: a conversation in active use stays legacy for as long as the person keeps sending in it (each send is an
old turn; nothing is wrong meanwhile, it is served through LU). An old turn with an open AskUserQuestion or
ExitPlanMode holds nothing (DECISIONS 17, claudecodeui fd1b3ed5): once it waits only on the question the old runtime
parks it (the turn ends for the client, Claude Code stops, the question stays in `jarvis_open_questions` and on the
person's screen), its old sandbox idles out, and the conversation is handed over with its question, which the person
answers on the new stack (the answer resumes the conversation, supervisor 7.6). Only a question answered on the old
stack whose turn has not started yet keeps the conversation legacy, for the seconds until that turn runs. `watch`
lists each legacy conversation with its owner and what it waits for (`in use`, `hold`, `old sandbox warm`), so Claude
can say who holds A5 up.

### 9.5 Running turns, asks, attachments, view-as, shares

- **Running old turns never cross.** They finish on the old stack. The move closes client sockets only; the person's
  CloudCLI child and its `/proc` connection to the sandbox (whose loss is what makes the old agent kill Claude Code)
  are untouched, so a turn running at the move keeps running and the reconnected tab sees it live through LU, with
  Stop through a ticket. No process migration, no `legacy.cut`, no deadline; the bounds are the old system's own
  (30 min hold ceiling, 8 h sandbox lifetime).
- **Asks.** An old turn's ask reaches a moved tab through LU (`agent_permission_request` and `_resolved`, in the
  shape the native feed uses), and the conversation's agent state there is the old stack's until its release: LU
  subscribes to the old child's list, and every agent snapshot pigeon sends that socket of a legacy conversation
  (list page, change-feed upsert, LU answer) is magpie's projection with the old snapshot's live state over it
  (status, `activeTurn`, `pendingPermissions`, attention), so the question card and Stop show and Stop's resolution
  clears the card. A change of the old state is pushed as an `agent_update` upsert with no `generation`/`seq` (not a
  list change: magpie's seqs stay the list's only positions). Passing the old `agent_update` through was rejected
  (magpie's own upserts of the conversation, the running mirror's every 5 s, would say "no turn" after it, and it
  needs a seq the list never had), as was mirroring the turn and asks into magpie (an ask up to 5 s late, ask rows
  for a turn magpie does not run). pigeon `legacy/view.rs`; WP-54's ask persona. An old turn that asks is never
  released mid-ask (it is running). After release asks are magpie's native, answerable flow.
- **Background tasks** of an old turn (`run_in_background` shells, Monitor, background agents) keep the conversation
  in its old child's running list for as long as they run: CloudCLI's `listRunningSessions`
  (`server/modules/providers/services/sessions.service.ts`) lists every session with tracked background work, and the
  30 min `BG_WAIT_CEILING_MS` hold (`claude-runtime.provider.js`) is a silence timer whose expiry is what closes the
  CLI's stdin and makes it end its background shells and agents. So no tracked background task outlives the
  conversation's presence in the running sets, and every rule here that waits for an empty running set (the release,
  `restart`) waits for them too; the A1 rollout waits for zero children, which is stronger. A process a turn detached
  from Claude Code's process group (`setsid`) is not killed by a child stop: it lives until the old sandbox's idle
  release, as on the old stack today.
- **Attachments**, by when the file was attached (under the old route's limits, 30 MB and 10 files, while any
  conversation is legacy, so every accepted file is acceptable to both stacks):
  - *before T0*: the file is in the person's `cloudcli-assets/` and is ingested into `import_uploads` (from A2, every
    5 s, and once more inside each release);
  - *from T0 to the gate* (an unmoved old-path socket may still send it): pigeon answers `POST /api/assets/files` and
    the image route as the old route does. It writes the file into the person's old child through LU's upload route,
    returns the old asset path, and records the same bytes in magpie (`uploads.legacy_path` = that path). The composer
    therefore holds an old path that works on whichever path receives the send: the old child accepts it on the old
    socket and through LU, and magpie resolves it for a native conversation. If the LU upload fails, pigeon answers
    with the old route's error (the failure the old path would have shown; `pigeon_upload_tee_failed_total`, a zero
    counter), never with a magpie-only path. This is switch-final C14.3's upload tee, limited to T0→gate;
  - *from the gate*: no old-path socket exists, so pigeon stores uploads in magpie only. A send of magpie-only files to
    a legacy conversation opens its ticket first, then copies the files into the old child through LU's upload route
    and sends with the old paths, all under that ticket (no old-side write outside a ticket). If the ticket call
    answers that the conversation is now owned (the fence won), pigeon drops the old paths and sends natively with the
    composer's magpie upload ids; the copies left in `cloudcli-assets/` are ingested and never referenced;
  - *resolution in magpie (M3, M7)*: a `files[].path` with the old asset prefix resolves through
    `uploads.legacy_path`, then `import_uploads`; on a miss magpie has the importer ingest that one file synchronously
    from `users/<key>/claude/cloudcli-assets/` (the import Deployment's `POST /v1/ingest-upload`) before it refuses
    with 400 `unknown_upload`, so a file attached a second before the move and sent after it to a pigeon-born
    conversation resolves. Every send makes a fresh upload copy (a new blob ref), since CloudCLI reuses an asset path
    any number of times (switch-final F44). `import-done` runs a last full ingestion first (nothing writes
    `cloudcli-assets/` after A5); the legacy-path resolution stays until A7 (drafts keep attachments for weeks).
- **View-as.** pigeon verifies old-router view-as tokens with the derived legacy key and re-signs LU calls with it,
  so an admin's view-as tab of a legacy conversation keeps working after the move (switch-final 11).
- **Shares** of a legacy conversation are viewed from magpie's imported copy; create and revoke go through LU;
  `continue` is routed by where the share lives and waits for hermit's clone.
- **Tokens.** No promise is made about a person's Access session ending inside the window; the behaviour is
  specified instead. Preflight lists the persons whose newest token expires before T0 + 8 h. LU is reopened only when
  the person's tab reconnects to pigeon, which goes through the edge and so carries a token the edge has just
  accepted (pigeon rollouts held and the HPA pinned, 9.3). If an LU upgrade or call still gets a 401 (an expired
  token on a socket that outlived it), pigeon answers that request with its normal signed-out contract (the tab shows
  the sign-in prompt, as when the edge finds the session expired), never a write error; `lu_401_total` alerts, and the
  person's legacy conversations keep their state for after the sign-in (switch-final C17).
- **The old purge** (the per-child Recently-deleted purge and `purgeWake`) is off from the A1 rollout (switch-final
  C9), so the old stack never removes the files of a conversation magpie owns.

### 9.6 The old side, the A1 rollout and the freeze

- **One ordinary ui rollout, in A1** (decision 11). A claudecodeui commit adds the switch module to
  `jarvis/router.mjs` (about 300 lines plus cases in `jarvis/test/router.test.mjs`): LU sockets recognised only by
  pigeon's pair token in `x-aviary-lu` (constant-time compare, header stripped before the child); outstanding request
  ids and the last inbound frame time per first-hop socket from a bounded-prefix frame reader that runs after the
  bytes are piped (client `requestId`, cleared by the matching response or `rpc_error`); **opaque sockets**
  (switch-final 4.2): a socket whose 101 negotiated an extension (expected never: CloudCLI's `ws` server keeps
  `perMessageDeflate` off, its default, `websocket-server.service.ts`), or whose reader met a frame it cannot follow (a
  continuation it lost, a `requestId` beyond the prefix, a reader exception) becomes `opaque` for its lifetime: it
  counts as busy, is never closed by `move`, keeps the gate unset and alerts (`switch_opaque_sockets`, expected 0). A
  byte stream cannot be resynchronised safely, so the runner's only ways out are the tab closing it (a reload, an app
  backgrounded and killed) or Deyao accepting one close of it (14); in-flight non-LU HTTP requests per person;
  `GET /__jarvis/sockets` (per person: non-LU sockets with outstanding count, last inbound time and `opaque`, LU
  sockets, in-flight non-LU HTTP; `generatedAt`); `POST /__jarvis/move [?person=]` (9.3) with the refusal of new
  non-LU upgrades and the pass-on of non-LU HTTP to pigeon from the first call, `POST /__jarvis/admit` undoing them;
  `POST /__jarvis/restart?person=` (rollback only: stops that person's child if its running set, holds and background
  work included, is empty and no LU request is outstanding, else answers `busy`; the next LU request starts a fresh
  child); the old purge and `purgeWake` off by env. The importer's and pigeon's pair tokens reach the router through `ui-env` (re-sealed in the same
  commit, since the rollout restarts the pods anyway). Nothing else changes in the old stack: no raised child cap (LU
  uses the same one child per person the old path did), no live config, no reload hook.
- **Nothing in flight is cut by it.** The old pods' `drain()` is not request-aware (it stops every child, destroys
  forwarded sockets and calls `closeAllConnections()`), so the rollout is gated on the old pods holding nothing at
  all, checked at the instant it matters. The A1 ui commit (`08-ui-rollout`, a Flux commit on the old cluster, 15.3)
  carries `spec.paused: true` and `maxSurge: 2` for this one revision (two surge pods of 1536 Mi beside the two old
  ones, one per 8 GB node; `ui-rollout` checks the room before it pushes). Flux applies the Deployment paused (the
  template changes, no pod does); at the quiet moment the runner suspends Kustomization `yyc-ai` and runs
  `cutover ui-gate` from the session, which
  (1) waits until both old pods have had no running work for 10 min (E5): by exec, every CloudCLI child's switch busy
  route (`GET /api/jarvis/switch/busy`, in the live ui image since claudecodeui 55090305: running turns and tracked
  background work, Claude Code processes held open, sandboxes being made, Paseo messages not yet completed, what
  `magpie import holds` reads); a child that cannot tell, or a process whose environment cannot be read, counts as
  busy. Connected tabs and idle children are not waited on: the drain closes those sockets and stops those children,
  and each open tab reconnects once (decision 11); (2) re-checks it and resumes the rollout; (3) every 2 s re-checks
  each old pod not yet terminating and pauses the rollout again if running work appears on it, resuming once (1)
  holds again. If a chat runs on a pod already
  terminating, the gate reports it (its 900 s drain lets it finish unless it runs longer). The gate uses the session's
  admin access to the old cluster (`cluster.sh ssh`); when the rollout is complete the runner pushes `09-ui-unpause`
  (git drops `paused`) and resumes the Kustomization, whose reconcile changes nothing. A1 is dark work, so decision 9 does not bind it; in practice
  the moment is an evening or night. There is no upper bound: if no such moment comes within 48 h of the push, the
  runner tells Deyao and keeps waiting.
- **Reconnects in A1.** Every tab open on an old pod reconnects once, to a new pod: with `maxSurge: 2` both new pods
  are ready before an old pod gets its SIGTERM, and a terminating pod leaves the endpoints. Only when the gate pauses
  the rollout between the two old pods (work started on old pod B) can a tab of old pod A land on B and reconnect a
  second time when B drains. The residuals, stated in 14: work that starts on an old pod in the ≈ 2 s between the
  gate's last check and that pod's SIGTERM can be cut by its drain (the job counts the running work it saw at each
  SIGTERM, `a1_rollout_cut_total`, expected 0); and an HTTP request in flight at an old pod's SIGTERM (an upload
  being sent) is cut by the drain, which is not request-aware, and the previous image cannot report one (the switch
  module that counts in-flight HTTP arrives with this rollout). The persona rig's A1 persona asserts at most one
  reconnect per tab and no cut turn (9.9).
- **Freeze from A1 to A5** (`cutover freeze-check` in `yyc-ai.yml`'s `check` job): armed by the A1 ui commit itself,
  which adds `yyc-ai/cutover/window.json`; the check binds every commit whose parent has `window.json`, so the commit
  that arms it (and changes `ui.yaml` and `ui-env`) is not checked against itself. From then on no commit touches
  `ui.yaml`, `jarvis-config`, a `yyc/secrets-rev` annotation in `legacy/`, `db.yaml` or the old connectors' manifests
  except the runner's own `10-`/`20-`/`30-`/`40-` patches, so no second ui rollout happens. Allowlist, admin, budget and model edits are refused
  from A3 to A5 (hours); `catalogue.json`'s users and admins equal `config.json`'s (a CI step). No node of either
  cluster is drained, upgraded or rebooted in A3-A5 (the node-work hold, 15.2 N8).
- **ui and `pigeon-app`** stay at the A1 claudecodeui commit until A5 (LU carries the old child's frames into
  pigeon-served pages).
- **Web chunks.** `pigeon-app` is seeded with the static chunks of every ui image served within the 720 h Access
  session lifetime (from the ui pin history) and `clients.txt` lists their commits; old-build chunks stay 30 days
  after the last old-build tab (pigeon counts `appVersion` from `hello`). No build-determinism gate: the chunks are
  copied from the images themselves.

### 9.7 Phases

| Phase | Extends | Work | Done when |
|---|---|---|---|
| A0 build and prove | cutover P0 | cutover P0; pigeon `legacy/`; magpie window routes and tables; the importer's hand-over; the router switch module (claudecodeui) with its tests; LU contract rig; persona rig and legacy-in-a-box nightly; fence-race fuzz and crash cases. Live, before anything else: the `legacy/` folder move on the old cluster's Flux (WP-39, 15.4) and, after the Talos session's steps 1-2, the new cluster's footing for YYC AI: the deploy path to the new cluster, its cluster-level objects, `talos-ready.sh` (WP-59, 15.2) | every CI green; legacy-in-a-box green on the commits A1 will deploy; `talos-ready.sh` green on N1-N12; a no-op commit gives a green `yyc-ai.yml` run against the new cluster with an empty plan |
| A1 new stack dark, the ui rollout | cutover P1 | P1 commits (the new stack by `yyc-ai.yml` on the new cluster, the old-side pieces by Flux on the old cluster; `magpie-pg` and `pg-shared` by the new cluster's workflow in commit 1), the bridges B1-B7 (15.5, `bridges.sh up`), NetworkPolicies `jarvis/ui-from-bridge` and the `yyc-ai` egress rules, store keys `YYC_TOKEN_PIGEON_UI`, `YYC_TOKEN_MAGPIE_IMPORT_UI`, `YYC_PIGEON_LEGACY_VIEW_AS_KEY`; the ui commit with the switch module, which also arms the freeze (`window.json`), applied by the gated rollout at a moment with no running work on the old pods (9.6, E5) | test host serves; fabric acceptance (hermit 24.3); LU contract passes against the production old router for bot; both ui pods answer `/__jarvis/sockets`; `a1_rollout_cut_total` 0 |
| A2 import and rehearsals | cutover P2 | bulk import and `verify --all`; render parity (cutover 10.2 item 6 with its allowed differences); `start-handover` (Deployment `magpie-import`: it syncs and ingests uploads and releases nothing before a gate, so staff see nothing; R1's per-person gate, R3 and the rollback export need it); upload ingestion running; rehearsals R6 (bridges, 15.5), R0, R1, R3, R4 | every rehearsal passes with every zero counter at 0 (a few days elapsed) |
| A3 flip and move | cutover P3 | whenever ready, no time window: preflight (the hand-over Deployment from A2 green), the switch commit (with the pigeon HPA pin), settle, `move` until every ui pod reports 0 non-LU sockets and 0 in-flight non-LU HTTP for 60 s, old connectors to 0 (moving continues), the gate | gate set |
| A4 hand-over | — | automatic: idle conversations release in the first pass, the rest as their old turns end | every conversation owned |
| A5 retire old | cutover P3 | preconditions held 30 min (every conversation owned, zero fenced, zero LU sockets, no old sandbox not `gone`, no old egress call in 30 min); then ui 0, old web/egress/tools 0, each a YYC commit; the bridge (import Deployment, NetworkPolicies, egress copy) stays up | old stack at 0; `verify --all` green |
| P4 (no wait, Deyao 2026-11-07) | cutover P4 | read once after A5: no open staff-data defect, no unplanned restart since A5, spend parity with OpenRouter within USD 0.50/day | as cutover 10.4 |
| A6 delete | cutover P5 | `import-done`; final dumps; the old chat UI's archive dump (decision 5); old fabric down; the teardown commits (old cluster: `yyc-ai/legacy/` deleted except `legacy/catalogue/`, Flux prunes; new cluster: the window objects with `Deploy-Prune` trailers); `bridges.sh down` for the links whose callers are gone (B1, B2; B4-B6 at their T4 moves; B7 is not bridged); tofu teardown (`ws`, `__jarvis`, tunnel `yyc-ai`); drop `jarvis` and `cloudcli`; contraction release (pigeon first, then magpie) removing the window code except the legacy-path resolution; code deletions; GHCR; store keys | cutover P5 checklist |
| P6 +30 days | cutover P6 | `hermit legacy-cleanup --confirm`; final dumps expire (the archive does not) | only `ws/` remains |
| A7 late-path cleanup | — | at A5 + 90 days, once no send carried an old asset path for 30 days: a contraction release drops `import_uploads`, `uploads.legacy_path` and the resolution in M3/M7; old-build chunks leave `pigeon-app` 30 days after the last old-build tab | `git grep -n 'legacy/\|x-aviary-lu\|import_uploads\|legacy_fence'` in aviary empty |

From A3 to the end of A5 is expected to fit one working afternoon: the flip and settle about 15 min, the move a
minute or a few (a person typing is moved once they pause for 30 s), the hand-over as long as the longest running
old turn plus its old sandbox's 30 min idle release, A5's 30 min of preconditions. That is the expectation, not a
bound: a legacy conversation in continuous use holds A5 (never A4's other conversations) until it goes quiet, is
stopped, or reaches the old sandbox's 8 h lifetime (9.4; an open question holds nothing, it is carried over); an opaque or
never-quiet old socket holds the gate (9.3). `watch` names each with its owner; nothing is read-only meanwhile.

### 9.8 The switch day (`cutover` runner in `YYC/yyc-ai/cutover/`, Rust, resumable)

T0 is any time once A2 is done (no time window). Claude runs the runner and watches its state; Deyao does nothing.
Every step that changes a cluster is a commit to DE0CH/YYC `main`, applied by `yyc-ai.yml` (new cluster) or by Flux
Kustomization `yyc-ai` (old cluster, `yyc-ai/legacy/`); the runner waits until `yyc-deploy/inventory-yyc-ai` records
that commit (or a descendant) with phase `done` and the run is green, or until the Kustomization is Ready at that
revision (15.3).

| Step | Does |
|---|---|
| `freeze-check` | A1 to A5: the window freeze in `yyc-ai.yml`'s `check` job |
| `ui-rollout`, `ui-gate` | A1: `ui-rollout` pushes the ui commit (`spec.paused: true`, applied by Flux on the old cluster) and, once both pods have had no running work for 10 min, suspends Kustomization `yyc-ai` and runs `ui-gate` from the session, which resumes the paused rollout only while no old pod's children report running, held, preparing or pending work (the switch busy route; connected tabs are not waited on, E5), and pauses it again before an old pod's SIGTERM if any appears (9.6); then `09-ui-unpause` and the Kustomization resumed (15.3); tells Deyao after 48 h without such a moment |
| `preflight` | cutover 11.2 without the night-time and "old connectors 0" items, plus: both ui pods' `/__jarvis/sockets` answering, pigeon's `/internal/switch/activity` answering, the persons whose newest token expires before T0 + 8 h listed (9.5), `import_uploads` caught up, warm hints flowing, new-egress budget headroom ≥ USD 10, AWS vCPU headroom for both fabrics' peak, the last `yyc-ai.yml` run green with an empty plan and Kustomization `yyc-ai` Ready on the old cluster, the freeze intact (no ui or old connector restart since the A1 rollout), both configs' allowlists equal, `bridges.sh check` green (every bridge target healthy where a callee pod runs), the node-work hold of 15.2 N8 set in `window.json` |
| `start-handover` | A2 (cutover 10.2 step 5): Deployment `magpie-import` (2 replicas, leader-locked); before a gate it only syncs |
| `flip` | the switch commit (DNS of `origin_ai` to tunnel `pigeon`, catalogue readers on `catalogue.json`, the monitor on hermit's snapshot; `ws` and `__jarvis` stay on both tunnels' path lists until A6); waits for `cloudflare.yml` and `yyc-ai.yml`; probes from an HK vantage until 20 consecutive answers carry pigeon's `aviary-request` header (Cloudflare replaces `server`); the settle time (10 min) |
| `move` | arms the refusal of new non-LU upgrades and the HTTP pass-on, then every 10 s `POST /__jarvis/move?person=` on every ui pod for each person not typing or attaching (9.3); continues until the gate is set |
| `old-connectors-0` | once every pod reported 0 non-LU sockets and 0 in-flight non-LU HTTP for 60 s: `patches/20-old-connectors-0.patch`; waits for 0 pods while `move` keeps running |
| `allow-handover` | the gate (`--person` for rehearsal R1 only) |
| `watch` | state every 5 s: conversations per state and what each waits for, LU sockets, the zero counters (9.9); any non-zero counter holds new fences and Discords Deyao |
| `retire-old` | A5, after its preconditions held 30 min: ui 0, old web/egress/tools 0 |
| `import-done` | after P4: `verify --all`, then the import Deployment, the NetworkPolicies and the egress copy are removed |
| `rollback …` | 9.10 |

### 9.9 Proving it

- **Zero counters**, from 5 min before T0 to the end of A5, measured live and alerted by the runner:
  `switch_inflight_closes_total` (a move close with a request outstanding, or a request frame read after the close
  decision), `switch_opaque_sockets`, `pigeon_upload_tee_failed_total`, `a1_rollout_cut_total` (A1 only),
  `pigeon_switch_write_refused_total`
  (any write refused or dropped because of the switch), `pigeon_lu_errors_total` (and no LU error surfaced to a tab),
  `pigeon_lu_503_total`, `lu_401_total`, `old_turn_cut_total` (an old child stopped with a chat running, from the
  router's log), `magpie_unknown_upload_total{legacy="true"}`, `magpie_conversation_migrating_total` reaching a tab,
  `import_verify_mismatch_total`, any synchronous wait over 3 s on a write, edge 5xx and tunnel errors for the host;
  `magpie_fence_seconds` p99 ≤ 0.3 and max ≤ 3. Counted, not zero: reconnects per tab at the move (expected 1) and
  replacements at release (at most 1 per subscribed socket and conversation).
- **Persona rig** (the instrumented web build and the iOS phone replica, in CI and in rehearsals): every sent message
  accepted once and shown once; no rpc error on any write; the composer's draft text and attachments survive the move
  and the replacement; a turn running at the move streams to its end and Stop works on it; a file attached before the
  move reaches the agent after release; an upload after T0 on an unmoved old tab, sent on the old socket, is read by
  the agent; a file attached at move − 2 s and sent at move + 3 s to a pigeon-born conversation reaches the agent; an
  ask in an old turn waits and Stop from a moved tab ends it; at most one reconnect per tab per move; a tab open during
  the A1 rollout reconnects at most once and a turn running at it is not cut (the gate waits for it).
- **Legacy-in-a-box nightly**: the ui image of the A1 commit with the new stack in kind, an edge stand-in for the DNS
  change (including a stale location that keeps routing to the old tunnel after the move: its upgrade gets the 503,
  its upload reaches pigeon), every persona above plus a hold, an idle warm old sandbox, a send in flight at the move,
  an admin's view-as tab and the A1 gated rollout from the previous ui image, web and iOS; green on the commits A1 and
  A3 deploy.
- **CI**: the ownership property tests with the invariants of 9.2; fence-race fuzz (1 000 sends at random instants
  around a fence, some with magpie-only files so the copy-at-send runs under the ticket while the fence commits: each
  accepted exactly once, never refused, user rows dense, the attachment read by whichever stack runs the turn);
  move fuzz (sends, Stops and steers issued at random instants around a move, with real network delay: each accepted
  exactly once or still in the composer, and the count of after-close frames equals `switch_inflight_closes_total`);
  crash cases (importer leader `kill -9`
  between fence and release, a pigeon replica with LU sockets killed, a magpie primary switchover during a fence, a
  stale ui report keeps the gate unset); the switch module's byte-identity and request-tracking cases, and
  `router.test.mjs` cases for a fragmented message, a continuation frame, a `requestId` beyond the prefix and a reader
  exception (each makes the socket opaque, never closed by `move`), the 5 s quiet rule, the 503 before the 101 after
  the first move, the HTTP pass-on and `restart` refusing a person with work running.
- **Rehearsals before A3**: R0 (established sockets survive the record change), R1 (bot holds every state at once:
  a long old turn, a hold, an ask, an idle warm old sandbox, a file attached but not sent, a web tab and the iOS
  replica, an admin's view-as tab; `move --person bot`, then the per-person gate; a release while a tab shows the
  conversation), R3 (reverse hand-over and forward again, with the person's old child holding the conversation cached
  so `restart` runs; and `rollback flip` before the gate in its order: creates through LU, pigeon-born conversations
  reversed once idle, then the DNS revert and `admit`), R4 (10 parallel conversations). No R5 (decision 12).

### 9.10 Rollback

| When | How | What staff see |
|---|---|---|
| A0-A2 | revert the phase's commits; the switch module is inert until `move` is called (reverting the ui commit would be one more gated rollout, 9.6) | nothing |
| A3, before the gate | `cutover rollback flip`, in this order: (1) `import_control.rollback_at` is set: pigeon routes creates through LU (born legacy) and keeps the T0→gate upload tee, so every composer holds old asset paths (9.5); (2) every pigeon-born conversation is reverse-handed-over (cutover 14.2) once it is idle: no running turn, hold or background task, no open ask; (3) only with zero owned conversations: revert `20-old-connectors-0` (if pushed) and the switch commit, then `POST /__jarvis/admit` on every ui pod; (4) pigeon closes its sockets request-free (its drain's rule plus the 5 s quiet of 9.3) and they reconnect to the old path. Rehearsed in R3 | one reconnect per open tab |
| A3-A5, after releases | per conversation behind pigeon (which keeps fronting), cutover 14.2: wait until the person's old child runs nothing; reverse fence in magpie; export (cutover 14.3 with switch-final C14.2's completeness: shares made on the new side, failed-turn rows, titles, per-conversation auto-memory into `/jfs/users/<key>/claude/aviary-memory/<conversation id>/`); if that child is up, `POST /__jarvis/restart?person=` (refused while anything runs: then unfence, the conversation stays owned and native, retried later); `import_sources` reopened, unfence into legacy; one replacement in the old child's epoch. The conversation is served natively until that instant, so its tabs repaint once; the restart also repaints the person's other shown legacy conversations once (the refresh decision 11 allows on a rollback) | at most one repaint per conversation |
| P4 | `cutover rollback old-up`: ui and the old backends back to their A4 values (revert `40-` and `30-`), then per conversation as above, behind pigeon; the DNS stays on pigeon. A fault in pigeon itself is fixed forward or by rolling pigeon back one image (its previous image still carries LU). A DNS revert is not part of the plan; only as a stated exception Deyao decides on, in this order: reverse every owned conversation through magpie and the writer sidecar (neither needs pigeon's serving path; each once idle as above), wait for zero owned, then revert the switch commit and `20-` together, after which every tab reconnects once to the old path | at most one refresh |
| after P4 | no rollback to the old stack; fix forward or roll one component back to its previous image (expand-only schemas) | — |

What each row does across the two clusters (which commits go through Flux, which through `yyc-ai.yml`, which bridge
carries what, and the loss of the new cluster as a whole): 15.11.

### 9.11 What decision 11 removed

Gone from the design, with their packages and tests: the router live patch (WP-53) and the in-place `SCM_RIGHTS`
upgrade; the router observer's heartbeat and visibility tracking, its fail-closed refusal, raised child cap and
LU-aware eviction, live allowlist/admin config and one-conversation reload hook; the move rule (hidden ≥ 30 s, two
hidden heartbeats, mobile/WebKit exemption, two-phase hold-down); the 72 h wait for a never-hidden tab; person states
S1-S4, presence reports and `person_switch_state`; proxy sockets and creates through LU; the seq remap, anchors, seq
maps, the release-time capture of the old in-memory timeline and the zero-difference parity gate; conversion only on
unsubscribed sockets; the list-level LU subscriptions; the ≥ 7 working days of socket measurement; the
build-determinism gate; the ui PDB `maxUnavailable: 0`; the "no tab shows it, no warm call in 60 s" release
conditions; `egress-export-jarvis` with its migration on `jarvis.egress_log` and role `egress_export` (WP-55);
rehearsals R2 (folded into R1) and R5.

The protections of switch-final 23 that are binding there, one by one (so a reader can tell which exist):

| switch-final | Here |
|---|---|
| C1.1 in-place `SCM_RIGHTS` upgrade | dropped; replaced by the gated A1 rollout (9.6) |
| C1.3 window freeze | kept (9.6), armed by the A1 ui commit |
| C1.4 pigeon HPA pinned, routine pigeon rollouts frozen | kept from T0 to the end of A4 (9.3) |
| C5.1-C5.2, C5.4-C5.5 ticket on outcome, quiet period, settle, re-stat | kept (9.4) |
| C5.3 the old router refuses writes to owned or fenced conversations | dropped: the gate needs the old path gone (no non-LU socket, upgrades refused before the 101, HTTP passed on to pigeon, old connectors at 0), so no old-path writer exists after a release (9.2-9.3) |
| C14.1 reverse into a child that cached the conversation | `POST /__jarvis/restart?person=` of an idle child, then the route change (9.10) |
| C14.3 every upload teed for the whole window; the old send path refusing unknown attachments | the tee kept from T0 to the gate (the only span with old-path sockets, and the span a rollback reverts to), copy-at-send under the ticket after it (9.5); the old send path is unchanged, since no composer can hold a magpie-only path while an old-path socket or a rollback to the old path exists |
| C15/F44 fresh upload copy per send, synchronous ingest on a miss | kept (9.5, magpie M3/M7) |
| C15 warm calls to hermit after the fence | kept from the gate (9.4) |
| C17 token lifetime | replaced by the specified 401 behaviour and the reconnect-only LU reopen (9.5) |

What is left to state to Deyao is in 14 (residual risks), not an exception.

---

## 10. What is deleted

| Where | Deleted | When |
|---|---|---|
| Cluster | namespace `jarvis` whole (ui, web, old egress, tools, cloudflared, their Services, PDBs, NetworkPolicies including `jarvis/ui-from-bridge`, Secrets including `ghcr-pull`), PV `yyc-ai-jfs` (volume kept), `kube-system/sbx-overlay-route`, RuntimeClass `gvisor`, the importer's ServiceAccount, Role and token Secret, the bridge Services `ui-bridge`, `pigeon-bridge`: on the old cluster, by Flux's prune when the A6 commit deletes `yyc-ai/legacy/` (except `legacy/catalogue/`, 15.4), then the Namespace by a `clusters/hetzner` commit (Flux `yyc-apps`). On the new cluster: the import CronJobs and Deployment, `ui-legacy`, `pigeon-bridge`, the `yyc-ai` egress rules to the bridges, by `yyc-ai.yml` from a commit that names them in `Deploy-Prune:` | A6 (`import-done` after P4, then the teardown commits) |
| Bridges (15.5) | the bridge server yyc-bridge1 with `yyc-bridge-old` and `yyc-bridge-new` (`bridges.sh down`), the NodePort and selector-less bridge Services with their EndpointSlices and policies, store key `YYC_OLD_CLUSTER_IMPORT_TOKEN` | B1, B2, B7 in A6; B4-B6 at the T4 move of their app; a load balancer with its last link |
| Cloudflare | tunnel `yyc-ai`, `pigeon-test`, paths `ws` and `__jarvis`, `ai-yyc-ui.deyaochen.com` (tofu and the hand-set CNAME) | A3 (`pigeon-test`), A6 |
| Postgres | databases and roles `jarvis` and `cloudcli` after locked final dumps. The old chat UI's data (database `jarvis`'s legacy tables of 2026-10-20/21: 37 conversations, 92 messages, 3 shares; bucket prefix `yyc-ai-files/jarvis/files/`, 38 objects, 3.3 MB) is **archived only, nothing imported, then deleted** (decision 5): a separate dump of those tables and a copy of the prefix under `yyc-pg-backups/archive/` (Object Lock, outside the dumps' expiry rule), then the prefix is deleted and the tables go with the database | A6 |
| Old fabric | EC2 instances `app=yyc-ai-sandbox`, launch template, AMIs, snapshots, security group, IAM users, gateway leftovers on `yyc-pool-apps-worker2` (old cluster; the server then goes to T2's rebuild), Hetzner route 10.200/15 and firewall | A6 |
| JuiceFS | `users/` (per-person `~/.claude`, shares), `sent/`, orphan `ws/` | P6 |
| DE0CH/claudecodeui | `jarvis/router.mjs` (with its switch module), `jarvis/Dockerfile`, router tests, `jarvis-image.yml`, the sandbox form of Jarvis mode in `server/modules/jarvis/*` (guard sandbox branch, view-as, shares, sent files, clone, purge, sandbox process), the Paseo bridge `server/modules/paseo` as a server for YYC; the gap-recorder flag of the web build unless kept as a generic test aid; Jarvis 4's local mode stays | A6 |
| DE0CH/jarvis3 | `chat/src/web/**`, `chat/sandbox/**`, `chat/sandboxd/**` (moved), `deploy/chat/**`, web parts of `chat/src/common`, `kv` announcements, `/x/<route>` paths, `/wire`, E2B code, `/x/sent-files`, `JARVIS_ROLE`, `JARVIS_CONFIG`, `JARVIS_SENT_FILES_TOKEN`, `sandboxd.yml`, the `import-jarvis` and export commands, docs of the removed parts | A6 (egress/tools changes land in A0; from then until A6 `deploy/chat/egress.yaml` pins the old stack's egress to `jarvis3-chat:f91907d349e6`, the last image of the old egress, and from WP-26 on `deploy/chat/tools.yaml` pins the old stack's tools to the same image (`src/main.ts` refuses `JARVIS_ROLE=tools` and `egress`); only web still starts from `JARVIS_ROLE`, so an old-stack jarvis3 bump ships web fixes only, no egress or tools change, the weekly allowlist refresh included; the pin rolls nothing back: `chat/src/tools` and `chat/src/common` are unchanged between YYC's tag 52c9b8c and f91907d; WP-25 and WP-26 reviews) |
| DE0CH/aviary | at A6: `crates/magpie-import`, `magpie import …`, `migrating` and its 409, pigeon's LU, tickets, fence, the copy-at-send of attachments into the old child, the legacy view-as key, the LU contract rig, the persona rig's switch personas and legacy-in-a-box nightly, hermit `import-jarvis`, `legacy-down.sh`; at P6 `legacy-cleanup`; at A7 the legacy-path set (`import_uploads`, `uploads.legacy_path`, the resolution in M3/M7) | A6, P6, A7 |
| DE0CH/YYC | `yyc/deploy/legacy/`, `config.json`, top-level `system-prompt.md`, `jobs/`, `yyc-ai-ami.yml`, the ZDR guard's old half, `yyc-ai/cutover/` (runner, patches, `freeze-check`), `infra/gvisor-node.sh`, old Secret rows, README sections on ui; the deployer's `jarvis` RoleBinding, the `kube-system` Role and the `gvisor`/`yyc-ai-jfs` names in `clusters/hetzner/yyc-ai-access.yaml` | A6 |
| GHCR | `yyc-ai-ui`, later `jarvis3-sandbox` | A6 |
| Relay (staff's own Claude Code on the model route; YYC README "Relay", jarvis3 650f6ff, live since 2026-11-02) | **Decision 14: disrupted (Deyao is its only user); no WP-58 in this plan.** Before decision 14: the new egress has no relay listener, no `relay_tokens` and no token page (web is deleted; egress.json routes are exactly `model` and `tools`). Keep = its own work package, WP-58 (an egress-owned token table, an admin API, a pigeon page, the `ai-relay` tunnel route to the new egress) landing before A3; drop = relay users told before A3, then `relay: true`, `relay.tf` and the `YYC_AI_RELAY_*` keys go in A6 | A3 (the old stack's relay works until A5) |
| Sandbox routes (decision 15; was "route MCP servers": an old-stack egress route with `sandbox: true`, jarvis3 adcd983, docs/CLAUDE-CODE.md "Skills and sandbox routes"; YYC's label store: route `labels` to `https://labels.yyc-internal.com`, the Access service token `yyc-labels-ai` injected, `X-Jarvis-Conversation` added, path `/api/v1/`, the REST API the `labels` skill teaches) | **Not deleted, carried by WP-61** (Deyao, 2026-11-03: YYC AI may change YYC's own internal data): the new egress has no `/x/<name>` paths, so the label store is the configured route `labels` (WP-61's choice: a generic configured route kind, platform 3.11.8; egress.json route `labels` with `inject`, `allowPaths: ["/api/v1/"]`, `identityHeaders` `X-Jarvis-Conversation`), the `labels` skill among the managed skills (WP-62, `yyc/deploy/hermit/skills/`), landing before A3; at A6 the old route, `JARVIS_ROUTE_*` and `JARVIS_EGRESS_TOKEN` go with web | A6 (the old stack's route works until A5) |
| Store `yyc` | `YYC_JARVIS_CC_TOKEN`, `YYC_JARVIS_SENT_FILES_TOKEN`, `YYC_CLOUDCLI_*`, `YYC_PG_CLOUDCLI_PASSWORD`, `YYC_JARVIS_DATABASE_URL`, `YYC_PG_JARVIS_PASSWORD`, `YYC_SBX_*`, `YYC_CF_AI_TUNNEL_TOKEN`, `YYC_SANDBOX_GHCR_DOCKERCONFIGJSON`, `YYC_AI_AMI_AWS_*`, the old scaler key, `YYC_TOKEN_PIGEON_UI`, `YYC_TOKEN_MAGPIE_IMPORT_UI`, `YYC_PIGEON_LEGACY_VIEW_AS_KEY` | A6 |
| claude-env | skill `yyc-ai` rewritten (names, runbooks, rollback, ops, the deploy workflow); scripts driving ui pods replaced by the ported probes; a lesson for the switch | A6 |
| Behaviour that goes | per-person CloudCLI processes and their LRU, rendezvous placement and peer holds, the `/proc` socket to the sandbox, the 60 s UI touch as idle signal, the shared per-person `~/.claude` (and with it the shared per-person auto-memory, decision 6), JuiceFS-backed `~/.claude`, the shared `JARVIS_CC_TOKEN`, egress reading hermit's tables, `kv` announcements, history folding at read time, failed-turn tables | with the old stack |

---

## 11. Deployment (YYC)

Where it runs (decision 13, section 15): the new stack on the new Talos cluster, the old stack on the old k3s cluster
until A6, Cluster `pg` on the old cluster until T5.

- **Namespaces** (new cluster): `yyc-ai` (PSS restricted): pigeon, magpie, hermit, egress, tools, cloudflared;
  `yyc-ai-fabric` (PSS privileged): `hermit-gateway` and `hermit-overlay-route` DaemonSets only. Both are cluster-level
  objects in `news-stream/deploy/clusters/talos/yyc-ai-access.yaml`, applied by the new cluster's own workflow, with the
  deployer's RBAC, so a `yyc-ai` commit can neither widen its own rights nor relabel a namespace.
- **Layout** `YYC/yyc-ai/deploy/` (the new stack only): `kustomization.yaml` (image pins: one aviary sha12 for
  pigeon/magpie/hermit/sandboxd, `pigeon-app`, `jarvis3-chat`; generators `hermit-config` (hashed), `catalogue`
  (unhashed, live reload), `egress-config`, `tools-config`, `legacy-kube-ca` (until A6); `secrets-rev` annotation per
  Deployment), `limits.yaml`, `network.yaml`, `cloudflared.yaml`, `pigeon.yaml`, `magpie.yaml`, `bridges.yaml`
  (`ui-legacy`, `pigeon-bridge`, `hermit-bridge`, the placeholders `yyc/yyc-mcp` and `yyc/web`; 15.5),
  `hermit/{hermit.yaml, fabric.yaml, juicefs.yaml, hermit.json, system-prompt.md, sandbox-rules.md, title-system.md}`,
  `catalogue.json`, `egress/{egress.yaml, egress.json}`, `tools/{tools.yaml, tools.json}`, `sealed/` (sealed for the
  new cluster), `jobs/` (until A6). No `namespace.yaml`, no `juicefs-csi.yaml` (a cluster add-on of the new cluster,
  `clusters/talos/juicefs-csi.yaml`) and no `legacy/`: the old stack is `YYC/yyc-ai/legacy/` (15.4), applied by Flux on
  the old cluster, with `yyc-ai/legacy/catalogue/` (the `yyc/yyc-catalogue` generator) while yyc-mcp runs there.
- **Configuration lives only in**: `hermit.json` (+ prompts), `catalogue.json`, `egress.json`, `tools.json`. The
  sandbox image digest, `fabric.sandboxd` and `fabric.runsc` are pinned in `hermit.json`; the AMI follows them.
- **Placement** (15.7): every YYC AI pod requires a node attached to network `yyc` (any eu-central location), egress
  an fsn1 one, `magpie-pg` one location for its three instances; two-replica Deployments spread by hostname with
  `DoNotSchedule`.
- **NetworkPolicies**: default deny; each component admits only its callers (platform 10). Sandboxes reach only the
  proxy port; hermit alone reaches :7070 and :7443. Callers on the other cluster arrive from a bridge's
  address, admitted as that /32 on that port only (15.5). In the new cluster's `db`: `magpie-pg-ingress` admits 5432
  by selector from the `magpie-pg` instances, the operator, magpie and the importers only, never the sandbox hosts
  (pg-ha 4.1, 15.8). On the old cluster `pg-ingress` gains 5432 from 10.0.0.0/16 for the bridged clients (B3) beside
  its 10.244.0.0/16 and 10.201.0.0/24 rules.
- **Databases** (decisions 2 and 10; `pg-ha.md` 4; 15.8):
  - Cluster `magpie-pg` for `magpie`, on the new cluster: 3 instances on three `pool=apps` nodes in one location (250m / 1 Gi
    each), synchronous `any 1`, `dataDurability: required`, `failoverQuorum: true` (no promotion of a possibly stale
    standby; with the primary and a standby lost together it waits for a manual promotion and magpie answers
    `unavailable`), `primaryUpdateMethod: switchover`, 10 Gi each, the same image as `pg`, nightly base backup 03:15
    HKT to `pg-pitr` under server name `magpie-pg`, daily dump `pg-dump-magpie`. Applied by the new cluster's workflow
    (`news-stream/deploy/clusters/talos/db/`).
  - Cluster `pg` for `hermit`, `egress`, `tools`, JuiceFS metadata `jfs_yyc_ai` and the news stream: **unchanged, one
    instance** (`pg-1` on `yyc-pool-db-worker1`, old cluster; decision 10) until T5. The new stack reaches it through
    Service `db/pg-shared` (selector-less, EndpointSlice `10.0.0.2:30432`, NodePort 30433), which T5 points at the new
    primary. NodePort `pg-hosts` stays `externalTrafficPolicy: Local` on `10.0.0.2:30432`; the old fabric's hosts dial
    it as today, the new fabric's hosts dial `<gw private IP>:30433` (15.6). Its restart is the stated gap of 7.3.
  - Daily dumps under Object Lock include all four new databases (`hermit`, `egress`, `tools` in the old cluster's
    `pg-dump-daily`, `magpie` in `pg-dump-magpie` on the new one).
- **Secrets**: Sealed Secrets from store `yyc` (`infra/seal.py` gains `A|B` lists for rotation and picks the cert by
  folder: `infra/sealed-secrets-talos.pem` for the new cluster, `infra/sealed-secrets.pem` for the old; 15.9), one
  Secret per component named after it; every image public, no pull Secret.
- **GitOps** (decision 1, `deploy-workflows.md`, decision 13): `yyc/deploy` is applied to the new cluster by
  workflow `DE0CH/YYC/.github/workflows/yyc-ai.yml`. Job `check` on every PR and push (render, `kubeconform`, the
  no-switch, zero-downtime, pin, secret, policy, catalogue and gateway-config checks, the ZDR guard, the deployer's
  RBAC table, the one-cluster check of 15.4, and during the switch window `cutover freeze-check`, which also watches
  `yyc-ai/legacy/**`); job `deploy` on push to `main`, every 3 hours against drift and on dispatch: plan against the
  live state, refuse any deletion a commit did not name in a `Deploy-Prune:` trailer, server-side apply in a fixed
  order (cluster-scoped, Sealed Secrets through the fenced applier `yyc-deploy/sealed-secrets-applier` with a wait for
  `Synced`, non-workloads, workloads), `rollout status` of every workload, prune, inventory
  (`yyc-deploy/inventory-yyc-ai`). The runner reaches the new API through tunnel `yyc-deploy` (connector on the new
  cluster) behind an Access service token and authenticates with its own GitHub OIDC token (identity `gha:yyc-ai`,
  accepted by the API server only from DE0CH/YYC `main`, environment `yyc-ai`, workflow `yyc-ai.yml`; the
  authenticator is in the control nodes' Talos machine config); no cluster credential is stored in GitHub, and the
  identity cannot read Secrets, create namespaces or touch RBAC outside its namespaces. The applier `yyc-apply`
  (Rust, `tools/yyc-apply/`) is also run from a session (`yyc-apply plan --trailers` through the new cluster's
  operator path, 15.2 N2) before any push that deletes or replaces infra. On the old cluster Flux keeps everything it
  has, including Kustomizations `yyc-ai` and `yyc-ai-secrets` for the old stack (now from `yyc-ai/legacy/`), until the
  Talos session removes the old cluster (T6); there is no hand-over. Tofu by `cloudflare.yml`. Other YYC workflows:
  `yyc-ai-zdr.yml` (daily), `hermit-ami.yml`.
- **Observability**: JSON logs, `/metrics` on 9090 with no scraper, the ops page, the news-stream monitor reading
  `/ops/snapshot` as the alerting path (through bridge B4 while the monitor is on the old cluster), a trace script by
  conversation id; a red `yyc-ai.yml` run (and the first green after it) goes to YYC's Discord channel.
- **New recurring cost** (Hetzner API, EUR with 20 % VAT): `magpie-pg`'s three 10 GB volumes, EUR 2.0592 per month
  (EUR 1.716 net; invoice line EUR 2.06; 2026-11-02), and, for the time the bridges exist (15.5), the bridge
  server yyc-bridge1 (cax11 fsn1), EUR 7.188 a month (EUR 0.01152 an hour; 2026-11-05). No node is added by YYC AI
  (decision 10 drops the HA plan's EUR 98.904); the new cluster's servers are the Talos plan's. The deploy tunnel and
  Access app cost USD 0.00 (Zero Trust Free); the workflow's Actions minutes are measured after the first runs against
  the plan's included minutes.
---

## 12. Residual inconsistencies between the specs, decided here

Each row says what a spec still contains and what holds. Implementers follow the "holds" column. On 2026-11-02
`spec-cutover.md` and `spec-platform.md` were re-edited for Deyao's decisions (the switch, the deploy workflow,
Postgres, the licence; last for decisions 10-12) and the component specs only for decisions 3, 5, 6 and 7; everything else a component spec
says about those topics is overridden by the rows D14-D23. On 2026-11-03 decision 13 (two clusters, section 15) was
folded into this document, `spec-cutover.md`, `spec-platform.md`, `spec-hermit.md` (the gateway), `deploy-workflows.md`
and `WORK-PACKAGES.md`; rows D26-D35 hold over any text that still describes one cluster.

| # | Topic | A spec still says | Holds |
|---|---|---|---|
| D1 | How the supervisor gets its sandbox token | supervisor 4.3 step 2, 6.2, 17.1, 23: from `HERMIT_SANDBOX_TOKEN` | platform R32, hermit 5.1: the token travels only as the proxy credential in `HTTP_PROXY`'s userinfo; the supervisor reads it from there, strips no other variable, and there is no `HERMIT_SANDBOX_TOKEN` |
| D2 | `/etc/hosts` entries for the fixed names | supervisor 4.3 step 4 and 17.1 keep "the four fixed names" lines written by hermit | no such lines exist (R32); the supervisor writes only `localhost` and `sandbox` |
| D3 | Token-level live frames | pigeon 8.7: "magpie has no `/live` route" | R36: magpie builds the receiving side (`POST /v1/sandbox/turns/{id}/live`, `live` in M9, `GET …/live`); the supervisor sends none and pigeon renders none in this release; streaming ships later behind a new `magpie-person` number |
| D4 | Turn origin enum | pigeon 22.1: `user \| cli \| import` | R30: `person \| agent \| import` |
| D5 | `reason` of `POST /v1/lifecycle/release` | supervisor 15.4: free text ≤ 200 | R34: hermit's `ReleaseReason` enum, plus `code`; a free `note` exists only on hermit's side |
| D6 | Gateway token lookup | magpie 3.3: `GET /v1/sandbox-tokens/{sha256}` | R33: local Ed25519 verification, liveness by `GET /v1/sandbox-tokens/{sandboxId}` |
| D7 | Who builds the sandbox image | supervisor 3: hermit's YYC implementation, in jarvis3 | platform decision 3: aviary `images/sandbox`, `ghcr.io/de0ch/hermit-sandbox:<sha12>`, supervisor and keeper from the same commit |
| D8 | Flush before a workspace clone | supervisor 15.5: "YYC does not call `prepare snapshot`" | hermit 3.8: YYC's hermit calls `prepare {kind: snapshot}` (a `sync(2)`) on a ready sandbox before a clone; failure is logged and the clone proceeds. The supervisor implements it as specified either way |
| D9 | The importer's run shape | magpie 19 and hermit 22: one mirror or catch-up loop | A2 bulk work as Jobs; the hand-over as Deployment `magpie-import`, 2 replicas, leader-locked, `magpie import handover`, from A3's `start-handover` until P4 ends (switch-final C14.3); no mirror loop |
| D10 | The switch order | platform 18 (now pointing here), magpie 19.5, hermit 22.3-22.5, pigeon 20, switch-final 16 | 9.7-9.8 (decision 11) |
| D11 | Old/new fabric overlap | hermit 22.2: at most 15 min | A3 to A5: a few hours (the longest running old turn plus its sandbox's 30 min idle release, then A5's 30 min); the old fabric runs hosts only for old turns |
| D12 | The target design's `GET /conversations/{id}/events?after=` | — | realised as M9 `/v1/conversations/{id}/timeline` over rows (R27); events stay the record (admin routes) |
| D13 | Readiness | any spec text that gates readiness on Postgres or LISTEN being up after start | platform 3.2 |
| D14 | Migrating conversations | pigeon 7.10 (retry 409 `conversation_migrating` for 45 s, then "being moved"), pigeon 7.9 `legacy.cut`, magpie 16.1/8.2 refusing every mutation of a migrating conversation, magpie 19's synthetic import turn | switch-final 4.1, 4.3, 8, 23 (9.2-9.5 here): a legacy conversation's writes go through LU with a ticket; native mutations of a fenced conversation wait ≤ 3 s; 409 `conversation_migrating` carries `route: "legacy"` and only re-routes a stale pigeon cache, never reaches a tab; no `legacy.cut`, no synthetic import turn, no "being moved" sentence. pigeon gains `crates/pigeon/src/legacy/` (A6) and the request-free-first drain (kept); magpie gains the window routes and tables of 9 (tickets, fence, `import_uploads`, `uploads.legacy_path`), not switch-final's anchors, presence or person states |
| D15 | How `yyc/deploy` is applied | any spec text saying Flux applies, reconciles or prunes `yyc/deploy`, `flux reconcile kustomization yyc-ai`, a `rollout restart` being undone by Flux | `deploy-workflows.md` (section 11 here): `yyc-ai.yml` applies; prune by trailer; a `kubectl rollout restart` now stays |
| D16 | magpie-pg manifest | magpie 18.2 | as written plus pg-ha 4: `imageName` = `pg`'s image, `enableSuperuserAccess: false`, priority `serving`, `failoverQuorum: true`, `nodeSelector pool=apps`, ScheduledBackup `magpie-pg-nightly` 03:15 HKT from a standby; ingress by address (`10.244.0.0/16` only), `pg-ingress` excludes `magpie-pg` |
| D17 | The hosts' JuiceFS metadata address | pg-ha 6: `10.0.0.5:30432` through `pg-hosts` with `externalTrafficPolicy: Cluster` | decision 10: `10.0.0.2:30432` as hermit 13.5 and platform `YYC_HERMIT_JUICEFS_META_URL` say; `pg-hosts` unchanged; the old fabric's `YYC_JUICEFS_HOST_META_URL` unchanged |
| D18 | Claude Code's auto-memory | magpie 6.1/19.1, cutover 7.4.2 (before this revision): `plan` fails on a memory file pending Deyao | decision 6: per conversation. A memory file found at import is copied into the cc scope of each of that person's imported conversations at its import and again at its release (each copy then diverges); nothing blocks the importer; a reverse export writes a conversation's memory to `/jfs/users/<key>/claude/aviary-memory/<conversation id>/`, which the old stack does not read |
| D19 | Window edits of people, budget and models | cutover D16-D17 keep `config.json` for the old stack; switch-final C11: live config in the old router | allowlist, admin, budget and model edits are refused by `freeze-check` from A3 to A5 (a few hours); `catalogue.json`'s users and admins equal `config.json`'s (a CI step); no live config in the old router (9.6) |
| D20 | Retention of the late-client set | switch-final 17: `import_uploads` and anchors deleted in A6; C10: anchors and seq maps until A7 | only the legacy-path set (`import_uploads`, `uploads.legacy_path`) exists and is kept until A7 (A5 + 90 days and 30 days without an old asset path); there are no anchors or seq maps (9.3) |
| D21 | How the A1 old-side changes are delivered | switch-final C1.1, 4.2, 16.1, 20 R3/R9: an in-place `SCM_RIGHTS` router upgrade; an earlier revision of this document: a live patch through Node's inspector | one ordinary ui rollout of a claudecodeui commit carrying the router's switch module, applied by the gated rollout (Flux applies it paused, the runner gates it with the Kustomization suspended, D28) while no old pod has running work (9.6, decision 11, E5); no live patch, no in-place upgrade |
| D22 | Tabs during the window | switch-final 5-7, 11, 12, 23 C4, C6, C7, C8 (observer, move rule, person states S1-S4, proxy sockets, seq remap, conversion only on unsubscribed sockets, list-level LU subscriptions, a never-hidden tab keeping the old stack up) | 9.2-9.3: every open old tab is moved right after the flip (request-free close, one reconnect), the old connectors go to 0, then one global gate; a shown conversation repaints once at its release |
| D23 | Shared Cluster `pg` | pg-ha 2-3 and 5-8, platform 11.5 and 16 as revised for decision 8 | decision 10: single instance as today (11, 7.3); only pg-ha 4 (`magpie-pg`, the `db` NetworkPolicies) holds |
| D24 | Old view-as tokens and reconnects in pigeon | pigeon 6.2 and 20 before the 2026-11-02 edit: old-router view-as tokens refused after the cut, a timeline `reset` on reconnect, the 7.10 migration retry | 9.3, 9.5 (pigeon 6.2 and 20 now say so): old view-as tokens are accepted through `PIGEON_LEGACY_VIEW_AS_KEY` while any conversation is legacy (the key is deleted in A6); no reset on reconnect, cursors stay valid until the one replacement at release; no 7.10 retry (D14) |
| D25 | How pigeon's socket messages are kept in step with the apps' zod schema | pigeon 18.1: hand-written serde types in `paseo::messages`, each type's fixtures serialised and validated against the exported JSON Schema with the `jsonschema` crate, and the schema's examples deserialised | (recorded by WP-17, 2026-11-02) frames are built as JSON in the schema's shapes and checked with the clients' own validators instead: a running pigeon driven by the newest web and iOS builds' client package and the app's timeline reducers, every frame as sent validated against every build in `clients.txt` (zod and zod-aot), plus the frame corpus of every socket test validated the same way; `schema/<commit>.json` stays exported. A zod JSON Schema export drops refinements and transforms, so it checks less than the validators the clients run |
| D26 | Where the new stack runs | any text naming the cluster's nodes (`yyc-pool-*`), `10.0.0.3:6443` as the new stack's API server, `pool=apps` nodes without a location, or the new stack and the old one sharing a cluster | 15.1-15.2: the new stack on the new Talos cluster (nodes in network `yyc`, egress leaving through yyc-nat1, 15.7), the old stack on the old cluster; the new API is reached as N1 states and matched in policies by entity (N3) |
| D27 | How the old stack is applied | deploy-workflows 8 (the Flux hand-over), WP-39 as the hand-over, cutover 10.0's "the deploy path and the Flux hand-over", platform 13.1's "Kustomizations `yyc-ai` and `yyc-ai-secrets` no longer exist", any `jarvis` object in `yyc-ai.yml`'s render | 15.3: Flux on the old cluster keeps applying the old stack from `yyc-ai/legacy/`; no hand-over; `yyc-ai.yml` renders only the new stack |
| D28 | The A1 gated rollout | 9.6 before decision 13, cutover 10.1 step 8, WP-35, WP-37: `yyc-ai.yml`'s deploy job, trailer `Deploy-Gated-Rollout`, the deployer's `pods/exec` | 15.3: Flux applies `spec.paused: true`, the runner suspends Kustomization `yyc-ai` and gates from the session, then `09-ui-unpause` |
| D29 | Addresses across the stacks | pigeon and the importer to `ui.jarvis:8080`; the pass-on to `pigeon.yyc-ai.svc:8080`; `jarvis/ui-from-yyc-ai` by pod selector; the monitor to `hermit.yyc-ai`; the importer to ui pod IPs; tools to yyc-mcp and capture by their in-cluster names | 15.5's bridges B1-B7; the switch module's token in `x-jarvis-switch-token` |
| D30 | The gateway | hermit 14.2's installer DaemonSet (`gateway.sh` through `nsenter`, a host systemd unit) on `yyc-pool-small-worker1`, route 10.202.0.0/15 → 10.0.0.5, `cluster.sh` guard | 15.6: sandboxd as the host-network pod's own process on one Talos node, data plane kept across restarts, route → `<gw private IP>`, the Talos upgrade workflow's guard |
| D31 | The new hosts' JuiceFS metadata address | D17 and hermit 13.5: `10.0.0.2:30432` | `<gw private IP>:30433` (15.6); the old fabric's `10.0.0.2:30432` unchanged |
| D32 | Databases | `magpie-pg` in Flux `yyc-db` on the one cluster; `pg-ingress` admitting 10.202.0.0/24 and excluding `magpie-pg`; URLs naming `pg-rw.db` | 15.8: `magpie-pg` on the new cluster (its workflow); `hermit`, `egress`, `tools` on `pg` (old cluster) through `db/pg-shared`; `pg-ingress` admits 10.0.0.0/16 |
| D33 | Sealed Secrets | one controller cert for everything | 15.9: a cert per cluster, chosen by folder |
| D34 | Repository layout | `yyc/deploy/legacy/`, `config.json` and `system-prompt.md` at `yyc/deploy/` until A6, the `yyc-catalogue` generator in `legacy/` | 15.4: `yyc-ai/legacy/` (Flux), `yyc/deploy/` new stack only, `legacy/catalogue/` until yyc-mcp's move |
| D35 | Node work during the window | 9.6's "no node drain is scheduled in A3-A5" for one cluster; `cluster.sh` as the only node tool | 15.2 N8: the Talos upgrade workflow and `upgrade-k8s` hold every node of both clusters and the Descheduler is suspended while `window.json` says `"nodeWork": "hold"`; the Descheduler never evicts in `db` and `yyc-ai-fabric`; N10-N11 keep the old cluster's footing until A6 |
| D36 | Where egress may run and how the new cluster leaves (Deyao, 2026-11-05: no primary IP limit raise) | 15.7 and N5 before 2026-11-05: egress requires fsn1, at least two workers in fsn1, every node with its own WARP `warp0` | 15.7: workers without public IP or WARP leave through yyc-nat1 (fsn1) via network `yyc`'s 0.0.0.0/0; egress requires network `yyc` only; the socks pool runs IPv4 only on the workers; `warp0` only on nodes with a public IP (control, gateway) |

---

## 13. Review status

Review rounds on the specs raised 69 issues in total. Every one is recorded where it landed or why it was not
taken: platform 23-24 (round 2, which added R26-R41 and the single gateway contract), magpie 23-24, pigeon 23,
hermit 29, supervisor 26-27, cutover 21-22. The switch-over was then redesigned for decision 4 (two competing
designs, `switch-A.md` and `switch-B.md`, merged in `switch-final.md`) and attacked: every attack scenario is closed
by a binding fix in switch-final 23 (clusters C1-C18, disposition map 23.14); one claim was corrected rather than
rejected (switch-final 24). The deploy path (`deploy-workflows.md`) and Postgres (`pg-ha.md`) were designed against
the live cluster on 2026-11-02. On the same day decisions 10-12 removed the HA plan for `pg` and let the switch reconnect
tabs once, and section 9 was cut down to what protects running turns, input, attachments and asks (9.11 lists what
went). The items still differing between spec texts are the twenty-four of section 12; none blocks
implementation. A further attack round on section 9 (2026-11-02, 29 issues) is closed in 9.3-9.11 and the specs it
names; the one rejected point (background tasks outliving the hold) is recorded in 9.5. Decision 13 (2026-11-03)
added section 15 and rows D26-D35; the Talos plan it rests on was read from its page and its session's transcript the
same day (nothing of the new cluster is built yet).
A review of section 15 the same day (13 issues, none rejected) added the build inputs handed to the builder before T1
(15.2.1), N12 and the window's three cpx32, Cilium's matching rules in N3 and the selector form of
`magpie-pg-ingress`, the sandbox data path on Talos (15.6), placement by network rather than fsn1 (15.7), the
Descheduler in N8, the full T5 change of `pg-shared` (15.8), the gap of losing the new cluster (15.11) and the split
of WP-60; the planning session is archived, so "the Talos session" now means the builder.

---

## 14. Open questions for Deyao

One, from the WP-07 gate (below, "Open from the WP-07 gate" 2). Otherwise, Deyao's decisions of 2026-11-02 (`DECISIONS.md`) settled everything the design asked: GitHub workflows for
YYC's deploys, `magpie-pg`, licence AGPL-3.0-or-later, the switch in working hours, the old chat UI's data archived
only, auto-memory per conversation, leavers purged only by an operator, the shared `pg` single-instance (decision 10,
which also removes the node-cost question), brief reconnects during the switch (decision 11, which removes the
questions of a never-hidden tab, of how the A1 changes reach the running old router and of a rollback into an old
child that cached the conversation) and no rehearsal R5 (decision 12). Two consequences are recorded rather than
asked: the A1 ui rollout runs at the first moment both old pods have had no running work for 10 min, which may be
outside working hours because it is dark work, not the switch, and has no upper bound (Deyao is told after 48 h,
9.6); and the old egress does not see new-stack spend during the few hours of the window (9.1, preflight headroom USD
10).

Open from the WP-07 gate (2026-11-02; `crates/supervisor/GATE.md`, supervisor 22 "Open items"):
1. **SIGSEGV after a live migration** (about 1 in 12 migrations with requests held in the supervisor, Claude Code
   once) and **a journal read with no whole request after a restore**: explained by WP-57 (2026-11-02). The SIGSEGV
   was the fork's (the final pre-copy round left freed pages with their old bytes on the target), fixed in fork
   release `precopy-20261102.1`; the short read was the scratch move's (rsync's quick check cannot see writes through
   a shared mapping), fixed in the gate's move by a checksummed final delta and open for sandboxd's `sync_scratch`.
   Both concern today's migrations too: today's runtime pin and sandboxd's move are not changed by WP-57 (operator
   phase). One acceptance batch on a fabric-AMI host in the fabric's account is left for WP-40.
2. **A question for Deyao: may the gate stand without an on-disk tombstone rewrite?** Claude Code 2.1.304 could not be
   made to remove a written line in any trigger reachable offline (supervisor 22 "Open items" 3). The proposal is
   to record the deferral as a decision: WP-22's re-run under supervisor 6.1-6.4 must trigger it on the mount (or show
   the version never rewrites a written line), and until then the tombstone row of supervisor 9.4 stays unverified.
   Until Deyao decides, WP-07 is not done.

Recorded consequences of decision 13 (section 15), not questions: the operator phase waits for T1-T2 and N1-N12
(15.2), so the switch's date follows that build; one Hetzner server (yyc-bridge1, HAProxy) bridges the clusters for the
window, EUR 7.188 a month while it exists (15.5); three new cpx32 in fsn1 give the new stack its room during the window, EUR
127.764 a month while they exist (N12, 15.7); the Talos steps 4-6 are ordered after A5 and A6 (N11); the loss of the
whole new cluster after releases is a stated gap (15.11).

For Deyao's next batch (an action, not a design question): the build inputs of 15.2.1 (CIDRs, CNI, API server flags
and SANs, node labels and network attachment, links, the node-work hold, the servers T2 must leave alone) are fixed
when the Talos cluster is first built, and the Talos plan page names none of them. The session that wrote that plan
(s4ba5e76eb6ec7ca4) is archived (transcript search, 2026-11-03: its record is `.jarvis4-records/2026-11-03 Kernel
patches on K8s runners.tar.gz`), so it builds nothing. 15.2.1 ends with a text ready to forward to whichever session
Deyao starts to build T1; alternatively his OK lets this session message that builder once it exists.

Residual risks, stated rather than claimed away (each counted live, each expected 0):
1. **A1 rollout**: work that starts on an old pod in the ≈ 2 s between the gate's last check and that pod's SIGTERM
   can be cut by the old `drain()` (`a1_rollout_cut_total`; 9.6), an HTTP request in flight at an old pod's SIGTERM
   (an upload) is cut by it, and a tab can reconnect twice when the gate pauses between the two old pods. It needs
   someone to send or upload in the seconds around an old pod's SIGTERM.
2. **The move**: a Send, Stop or steer pressed within the network delay of a close (well under a second), after ≥ 5 s
   with no frame on that socket and ≥ 30 s without a keystroke or attachment, fails with "failed to send"
   (`switch_inflight_closes_total`, the move fuzz; 9.3).
3. **Opaque or never-quiet old sockets** hold the gate until the tab closes; Deyao may accept one close of such a
   socket (9.3, 9.6).
4. **A P4 DNS revert** is a stated exception, not a planned path (9.10).

---

## 15. Two clusters (decision 13)

Text of record for everything decision 13 changes (Deyao, 2026-11-03): YYC's cluster moves to a new Talos cluster
with three control nodes, planned by the session "Kernel patches on K8s runners" (s4ba5e76eb6ec7ca4; plan
https://1e6d165c-page.deyaochen.com/, its steps T0-T6 below). The new stack deploys straight onto the new cluster
(the plan's step 3), the old stack stays on the old one, so the switch also moves YYC AI between clusters and YYC AI
moves only once. Where another section of this document or a spec still describes one cluster, this section holds
(rows D26-D35 in 12). The build is not this design's: this section lists what YYC AI needs from it (build inputs,
15.2.1, and checks, 15.2) and what changes in YYC AI. State on 2026-11-03: the plan is written and waits for Deyao's
review; nothing is built; the planning session is archived (transcript search lists it only as a
`.jarvis4-records/… .tar.gz` record), so the build, and the re-measure for the packing plan (T0) it had scheduled for
2026-11-06, fall to a session Deyao starts. That session is called **the builder** here; "the Talos session" anywhere
in this design and the specs means the builder. Its state is read from its own transcript (`transcript-search.py
"Talos" --after <its start>`, its session name), never by a message unless Deyao says so; until a builder exists,
nothing of T1 is assumed and the plan page is the only description of the cluster.

### 15.1 What runs where

| What | Old cluster (k3s `yyc`, one master `yyc-master1`, Flux) | New cluster (Talos, three control nodes, GitHub workflows) |
|---|---|---|
| Old stack: namespace `jarvis` (ui, web, old egress, old tools, connectors of tunnel `yyc-ai`), the old fabric's gateway `yyc-sbx-gw` on `yyc-pool-apps-worker2` | until A6 | never |
| New stack: `yyc-ai` (pigeon, magpie, hermit, egress, tools, connectors of tunnel `pigeon`, `magpie-import`, the import CronJobs), `yyc-ai-fabric` (`hermit-gateway`, `hermit-overlay-route`), `db/magpie-pg`, the deploy connector `yyc-deploy/cloudflared` | never | from A1 |
| Cluster `pg` (databases `hermit`, `egress`, `tools`, JuiceFS metadata `jfs_yyc_ai`, `jarvis`, `cloudcli`, the news stream) | until T5, which runs after A6 | from T5 (a CloudNativePG replica cluster, then promoted) |
| yyc-mcp, the web capture route (`web.yyc`), the news-stream monitor, the other YYC apps | until T4, which runs after A5 for these three | from T4 |
| JuiceFS volume `yyc-ai` (Hetzner Object Storage, metadata in `pg`) | mounted by the old fabric's hosts and the ui pods | mounted by hermit, `magpie-import` and the new fabric's hosts |
| AWS fabrics | old hosts behind the old gateway (unchanged) | new hosts behind the new gateway (hermit 13.1 ranges unchanged; 15.6 for the node) |

YYC AI moves between clusters in exactly two ways, both already part of section 9: the DNS flip at A3 sends new
connections to tunnel `pigeon` (new cluster) instead of tunnel `yyc-ai` (old cluster), and each conversation moves by
the per-conversation hand-over (9.4), whose state is in `magpie-pg` on the new cluster. Decision 11's guarantees are
unchanged: nothing in this section adds a reconnect, a read-only state, a cut turn or a lost input; the old stack
keeps its whole footing on the old cluster until A6 (N10).

### 15.2 What the new cluster must provide before the operator phase

The operator phase (WP-39, WP-40 onward; decision 13) starts only after T1 and T2 (control plane built and tested,
first workers) are done and every item below holds on the live clusters. Items N1-N9 and N12 are what the new cluster
provides; N10-N11 are what the old cluster and the order of the Talos steps keep. Most of them can only be set when the
cluster is first built (the Pod and Service CIDRs, the CNI, the API server's flags and SANs, the network attachment
and node labels): they reach the builder before T1 as the build inputs of 15.2.1, and `yyc/infra/talos-ready.sh`
(WP-59, read-only, output in the task record) verifies afterwards that the built cluster has them. A mismatch found
then means rebuilding T1, which is why the inputs go first.

| # | Requirement | Why | Check |
|---|---|---|---|
| N1 | The deploy path: the Kubernetes API reachable from the connector of tunnel `yyc-deploy` at `kubernetes.default.svc:443`; the API's endpoints (the `kubernetes` Service's EndpointSlice addresses and port) fixed and recorded; `127.0.0.1` among the API server certificate's SANs; the API server's `AuthenticationConfiguration` (deploy-workflows 4.1, same claim rules) supplied by the machine-config patch of deploy-workflows 4.1 (input I4: the file under `/var` by `machine.files`, the API server flag `authentication-config`, an `extraVolumes` mount), applied one control node at a time (the API never pauses); the Kubernetes CA committed as `infra/yyc-talos-ca.pem`. The API server fetches GitHub's JWKS over each control node's WARP link (`token.actions.githubusercontent.com` has no AAAA record, `dig`, 2026-11-03); with WARP down on a control node, that API server rejects new GitHub tokens once its cached keys no longer cover them, so about one in three deploy requests fails (red run, re-run) while workloads and the operator path (N2, client certificates) are unaffected | `yyc-ai.yml` targets the new cluster (15.3) | a plan-mode `workflow_dispatch` authenticates as `gha:yyc-ai` (`kubectl auth whoami`) and `kubectl auth can-i --list` matches deploy-workflows 12.3; then the same token is sent to each control node's own address (`--server https://<node>:6443`, through N2) and each answers `gha:yyc-ai`, which proves each API server fetched the JWKS through its WARP link; WP-59 confirms with `talosctl validate` and the release's docs that I4 is that release's mechanism |
| N2 | An operator path from a Jarvis 4 session to the new API (get, logs, exec, port-forward) with an admin kubeconfig from store `yyc`, the new cluster's counterpart of `cluster.sh ssh` | the runner, `yyc-apply plan --trailers` before deleting pushes, the gateway runbook | `talos-ready.sh` runs through it |
| N3 | Cilium as input I2 (Talos's default flannel enforces no NetworkPolicy). Its matching rules, which platform 10 is written to: an `ipBlock` selects only addresses outside the cluster (the old cluster's nodes, the bridge addresses, 10.202.0.0/15, the internet) and never a pod, a node or the API server of this cluster; pods are matched by pod and namespace selectors; the API server by the entity `kube-apiserver` (a `CiliumNetworkPolicy` with `toEntities`), never by address, so a replaced control node changes no policy | YYC AI's isolation is its NetworkPolicies (platform 10; plain HTTP in the cluster) | in a default-deny namespace with probe pods: a denied connection fails and an admitted one passes; an `ipBlock` /32 to an old node admits only that address; an `ipBlock` naming `<pod-cidr>` admits no pod (proof that pod rules must be selectors); egress to the API by `toEntities: kube-apiserver` passes, by an `ipBlock` of a control node's /32 it does not; through `magpie-pg-ingress`, a standby's streaming and a connection from an `app=import` pod pass and one from an unlabelled pod fails; a readiness probe passes under default deny |
| N4 | Pod CIDR 10.210.0.0/16 and Service CIDR 10.211.0.0/16 (input I1), disjoint from 10.0.0.0/16 (network `yyc`), 10.200.0.0/15 (old fabric), 10.202.0.0/15 (new fabric) and the old cluster's 10.244.0.0/16 (10 live Hetzner routes in network `yyc`) and 10.45.0.0/16, written in the cluster README (this design calls the pod CIDR `<pod-cidr>`); pods' traffic to 10.0.0.0/16 leaves masqueraded to the node's private address | `host.json`'s `podCidr`, the cloud controller's routes (I3); `pg` and the old API see a node address | every node's `podCIDR` inside 10.210.0.0/16; a probe pod's connection to `pg-hosts` arrives from its node's 10.0.0.x |
| N5 | At least three schedulable workers attached to network `yyc` (node label `yyc.hetzner/network=yyc` from the machine config, input I5), in any location of network zone eu-central (fsn1, nbg1, hel1), labelled `pool=apps` (and, for the gateway, N7), with N12's room; a fourth worker in `magpie-pg`'s location lets a drained instance move instead of waiting; every new server labelled `yyc-cluster=talos` in Hetzner (and every old one `yyc-cluster=k3s`, relabelled by T2's rebuild) | 15.7; the bridges target servers by that label (15.5) | `kubectl get nodes -L pool,yyc.hetzner/network,topology.kubernetes.io/region`; `hcloud server list -l yyc-cluster`; every node with the network label has a private IP in 10.0.0.0/16; egress leaves through yyc-nat1 in fsn1 (network `yyc`'s route 0.0.0.0/0 → 10.0.0.30, change of 2026-11-05 in 15.7) |
| N6 | Add-ons: Sealed Secrets (public cert committed as `infra/sealed-secrets-talos.pem`), CloudNativePG at the old cluster's operator version (the same operator also serves T5's replica cluster), the Hetzner CSI driver with StorageClass `yyc-block` (`volumeBindingMode: WaitForFirstConsumer`, so a volume is made in its pod's location), the Hetzner cloud controller (location labels), the JuiceFS CSI driver (`juicefs-csi.yaml`, moved to the new cluster's folder, 15.3) | `magpie-pg`, `hermit-jfs`, the sealed Secrets | a pod mounts volume `yyc-ai` read-only through the JuiceFS CSI on a Talos node and lists `ws/`; a `yyc-block` PVC attaches |
| N7 | The gateway node: one fsn1 worker in network `yyc` with its public IPv6 (fsn1: the AWS hosts are in eu-central-1, Frankfurt), label `hermit.yyc/gateway=true`; the new servers' Hetzner firewall lets udp 51821 in on it from any IPv6 address; the routes and links of inputs I3 and I6 (cloud-controller routes for the pod CIDR, KubeSpan without pod networks, Cilium in VXLAN tunnel mode, the MTUs, the WARP link `warp0`). If Talos's ingress firewall is on (it filters traffic to the node itself), it admits: on the gateway node udp 51821 from any address, tcp 7443 and 7444 from 10.202.0.0/24, 10.0.0.0/16 and `<pod-cidr>`, tcp 30433 from 10.202.0.0/24 and 10.0.0.0/16; on every new node tcp 30890 and 30893 from 10.0.255.2/32 (B2, B4: traffic and health checks), udp 51820 (KubeSpan) and udp 8472 (Cilium VXLAN) from the cluster's nodes, plus the builder's own Talos and Kubernetes ports | 15.6 | with Talos's firewall as built: the `hermit-gateway` pod (WP-19's image) creates `wg0` and its nft table; a test WireGuard peer reaches the VIP 10.202.1.1:3128 and the DNAT delivers to an egress pod on another node with the source kept (the pod sees 10.203.x); a 1 MiB TCP transfer each way from the peer through the VIP completes (MTU and MSS clamp); the egress pod's reply, source its pod address, arrives at the gateway on the private interface from 10.0.0.1 (`tcpdump` in the gateway pod); the peer reaches `<gw>:30433`; Hetzner's routes of network `yyc` keep 10.200.0.0/15, 10.202.0.0/15 and the ten old 10.244.x.0/24 routes after the new cloud controller starts and gain one 10.210.x.0/24 route per new node in the network; the bridge NodePorts answer from 10.0.255.2; after a kill of the gateway pod and a machine-config apply on its node without reboot, `wg0`, sandboxd's nft table, route table 51822 and its rule are unchanged, and KubeSpan's and Cilium's route tables, rules and fwmarks (`ip rule`, `talosctl get routes`) share none of sandboxd's |
| N8 | Node work honours YYC AI: the weekly Talos upgrade workflow and `talosctl upgrade-k8s` (run through the same workflow) upgrade the gateway node only through hermit's gateway-maintenance runbook (hermit 14.7), and touch no node of either cluster while `yyc-ai/cutover/window.json` has `"nodeWork": "hold"` (set by the runner's `preflight`, cleared when A5 is done). The Descheduler (the builder's CronJob in the 04:00 HKT window) evicts nothing during the hold: the runner's `preflight` commit that sets the hold also sets that CronJob's `suspend: true` in the builder's folder (one of the runner's own patches, so `freeze-check` passes it), and A5's commit clears both. Its policy excludes namespaces `db` (a CNPG instance's eviction is a failover and a Volume re-attach) and `yyc-ai-fabric` permanently (`evictableNamespaces.exclude` on every plugin) and keeps PDBs (its default) | 7.3 item 1; 9.6's "no node drain in A3-A5" now spans two clusters, and nightly evictions of pigeon, magpie, `magpie-import` or `magpie-pg` pods would add reconnects and write pauses decision 11 does not allow | a dry run of the upgrade workflow and of `upgrade-k8s` with the hold set skips every node; with the hold set the Descheduler CronJob is suspended; `descheduler --dry-run` with the live policy lists no pod in `db` or `yyc-ai-fabric` |
| N9 | The Sealed Secrets fence without Flux: ServiceAccount `yyc-deploy/sealed-secrets-applier` bound to ClusterRole `sealed-secrets-applier` (SealedSecrets only) in `yyc-ai`, `yyc-ai-fabric` and `yyc-deploy` | deploy-workflows 6.4 step 2 impersonates it | `kubectl auth can-i --as=system:serviceaccount:yyc-deploy:sealed-secrets-applier` |
| N10 | The old cluster keeps, until A6: `yyc-master1` (the API, link B7), `yyc-pool-db-worker1` (`pg`), `yyc-pool-apps-worker2` (the old gateway and fabric), `yyc-pool-large-worker1` and `-worker2` (the 8 GB nodes with room for A1's two 1.5 GiB surge pods: 3.9 and 2.0 GiB free on 2026-11-03, against 1.6 GiB on apps-worker2 and 0.4 GiB on db-worker1), Flux, and allocatable for the old side's peak: its live requests (26 684 Mi on 2026-11-03, including the whole old stack, which comes back on `rollback old-up` until A6) plus 3 072 Mi of surge plus the 2 Gi poller rollout slot, 31 804 Mi (31.1 GiB) of its 43 412 Mi allocatable. T2 takes only what is beyond that: on 2026-11-03 about three of the five cx23 workers, fewer unless T4 has moved their pods out first. This peak is an input to T0's packing plan | 9.6, 9.10 | `talos-ready.sh` reads the old cluster's allocatable and requests and checks the peak fits, with two 1.5 GiB slots on two different 8 GB nodes |
| N11 | Order: T4 for yyc-mcp, the capture route and the monitor runs after A5 (each move commit also changes its bridge row of 15.5); T4 for other apps any time outside A3-A5; T5 after A6 (a P4 `old-up` brings back old pods that read `jarvis` and `cloudcli` on the old cluster) and after T4 has moved every app that writes to `pg` (an app still on the old cluster writes to the old `pg-rw`, read-only from the demotion, and loses writes until it is pointed at the new primary); T6 after A6, T4 and T5 | 15.5, 15.8, 15.11 | the builder's task record |
| N12 | Room for the window on the new side: YYC AI's 13 824 Mi at A3-P4 (15.7) plus each node's own pods (Cilium, kube-proxy, Alloy, the two CSI node pods, the overlay route: about 600 Mi a node, Claude's estimate until T2's first node shows its allocatable), on at least three nodes that each fit a 1 536 Mi magpie pod and a 1 152 Mi `magpie-pg` instance. T2 cannot supply this from old servers (N10 leaves it about three cx23), so the window gets three new cpx32 in fsn1 (15.7), bought in WP-59 | 15.7 | `talos-ready.sh` sums allocatable minus requests on the nodes YYC AI may use: ≥ 13 824 Mi, and ≥ 2 688 Mi free on each of three nodes |

### 15.2.1 Build inputs, handed to the builder before T1

These are fixed when the Talos cluster is bootstrapped (or when a node is first configured), so they go to the
builder before T1, through Deyao (14: a text to forward, below, or his OK to message the builder). WP-59 owns them;
`talos-ready.sh` checks each against the built cluster (the N item named).

| # | Input | Value | Checked by |
|---|---|---|---|
| I1 | Pod and Service subnets | `cluster.network.podSubnets: [10.210.0.0/16]`, `serviceSubnets: [10.211.0.0/16]`, node CIDR mask /24. Not Talos's default pod subnet 10.244.0.0/16, which is the old cluster's: network `yyc` has 10 live routes for it (10.244.{0,1,2,3,4,6,8,9,10,11}.0/24 to the old nodes; Hetzner API, 2026-11-03) | N4 |
| I2 | CNI | `cluster.network.cni.name: none`, Talos's kube-proxy kept; Cilium installed by the builder with `ipam.mode: kubernetes` (node pod CIDRs from I1, matching the routes of I3), `routingMode: tunnel`, `tunnelProtocol: vxlan`, `kubeProxyReplacement: false` (NodePorts answer on every interface, `wg0` included), `enableIPv4Masquerade: true` with `bpf.masquerade: false` (pod traffic leaving the pod network, 10.0.0.0/16 included, is masqueraded to the node's address), `MTU: 1320` (I6), default policy enforcement, and the cgroup and capability settings of Talos's Cilium guide | N3 |
| I3 | Cloud controller | the Hetzner cloud controller with network `yyc`, routes on (`HCLOUD_NETWORK_ROUTES_ENABLED=true`) and cluster CIDR 10.210.0.0/16: each new node in network `yyc` gets a Hetzner route for its pod /24, as the old nodes have. The gateway's forward path (sandboxd's table 51822: `<pod-cidr> via 10.0.0.1`) and the egress pods' replies (source: the pod's own address) cross network `yyc` on these routes, never KubeSpan or VXLAN. Its route controller manages only routes inside its cluster CIDR, so 10.200.0.0/15, 10.202.0.0/15 and the old 10.244.x routes stay; a node outside network `yyc` gets no route and runs no YYC AI pod (N7 checks both; Claude's reading of the controller, unverified until WP-59 reads its source) | N7 |
| I4 | API server | `certSANs` include `127.0.0.1`; the authenticator patch of deploy-workflows 4.1 on the three control nodes; no `--oidc-*` flags (the API server refuses them beside `authentication-config`) | N1 |
| I5 | Nodes YYC AI runs on | attached to network `yyc`, with `machine.nodeLabels: {yyc.hetzner/network: "yyc", pool: apps}` on those only; every new server labelled `yyc-cluster=talos` in Hetzner; for the window three new cpx32 in fsn1 (N12, 15.7) | N5, N12 |
| I6 | KubeSpan and links | `advertiseKubernetesNetworks: false` (pod traffic between nodes rides Cilium's VXLAN, so a KubeSpan peer never sees a pod or 10.202 source outside its AllowedIPs); KubeSpan `mtu: 1370` (network `yyc`'s 1450 minus WireGuard's 80) and Cilium `MTU: 1320` (1370 minus VXLAN's 50; Claude's arithmetic); the WARP link named `warp0` (never `wg0`, which is sandboxd's), not on udp 51821, with an MSS clamp on SYNs forwarded out of `warp0` (`tcp option maxseg size set rt mtu`); no fwmark, route table or rule priority that sandboxd uses (table 51822; rule priorities 1000 `iif wg0 lookup main suppress_prefixlength 16` and 1001 `iif wg0 lookup 51822`, which sit after the local table's rule, `100: from all lookup local` on Talos, and before KubeSpan's 32500; no fwmark); Talos's ingress firewall off on the private interface or with N7's list | N7 |
| I7 | Gateway node | one of the fsn1 cpx32 of I5; label `hermit.yyc/gateway=true` set later by `gateway-net.sh` | N7 |
| I8 | Node work | the weekly upgrade workflow and `upgrade-k8s` read `yyc-ai/cutover/window.json` and skip while `nodeWork` is `hold`; the Descheduler CronJob carries `suspend` in the builder's folder (the runner sets it) and its policy excludes `db` and `yyc-ai-fabric` | N8 |
| I9 | Old servers and order | T2 never takes the servers N10 keeps; T4, T5, T6 in N11's order | N10, N11 |

Hetzner API `server_types`, 2026-11-03 (a fact for the builder, not a plan of its build): cx23 and cx33 are on sale
in no location (the plan assumed cx23 in hel1), cax11 and cax21 only in fsn1, cpx22 and cpx32 in fsn1, nbg1 and hel1.

Text to forward (Deyao to the builder):

> YYC AI's new stack lands on your Talos cluster (plan step 3). Before you build step 1, these are fixed at
> bootstrap and YYC AI needs them: pod subnet 10.210.0.0/16 and service subnet 10.211.0.0/16 (never 10.244.0.0/16,
> the old cluster's, which has live routes in network yyc); CNI none in Talos, then Cilium in VXLAN tunnel mode,
> ipam kubernetes, kube-proxy kept, iptables masquerade, MTU 1320; the Hetzner cloud controller with network yyc
> routes on and cluster CIDR 10.210.0.0/16; KubeSpan with advertiseKubernetesNetworks false and MTU 1370; the WARP
> link named warp0, MSS-clamped, never wg0 or udp 51821; API server certSANs with 127.0.0.1 and YYC AI's
> AuthenticationConfiguration file (machine.files under /var, extraArgs authentication-config, extraVolumes);
> nodes for YYC AI attached to network yyc with node labels yyc.hetzner/network=yyc and pool=apps, Hetzner label
> yyc-cluster=talos; three new cpx32 in fsn1 for YYC AI during the switch (cx23 and cx33 are sold out everywhere
> today); the upgrade workflow, upgrade-k8s and the Descheduler hold while yyc-ai/cutover/window.json says
> "nodeWork": "hold", and the Descheduler never evicts in namespaces db and yyc-ai-fabric; T2 leaves yyc-master1,
> db-worker1, apps-worker2 and both large workers on the old cluster until YYC AI's A6. The full list with reasons
> is DESIGN.md 15.2.1 of the YYC AI refactor (session sa23aea2f64db540a, ~/artifacts/refactor/).

### 15.3 Deploys on two clusters

- **The new stack**: `yyc-ai.yml` (deploy-workflows 5-6) applies `yyc/deploy/` to the new cluster. The runner
  reaches the new API through tunnel `yyc-deploy`, whose connector runs on the new cluster; the identity is the same
  GitHub OIDC `gha:yyc-ai` (N1); the kubeconfig's CA is `infra/yyc-talos-ca.pem`. The cluster starts empty, so the
  first run's plan is creates only and the inventory starts empty: there is nothing to seed and nothing to adopt.
- **The old stack stays on Flux** on the old cluster until the Talos session removes that cluster (T6): Kustomizations
  `yyc-ai` and `yyc-ai-secrets` keep applying it, from `yyc-ai/legacy/` and `yyc-ai/legacy/sealed/` (15.4). There is
  no Flux hand-over (the former WP-39, deploy-workflows 8, is dropped), so the old master's k3s restart for
  `master-authn.sh` never happens. The runner's old-cluster commits (`08-`, `09-`, `20-`, `30-`, `40-` and A6's
  teardown) are Flux commits: the runner pushes, runs `flux reconcile kustomization yyc-ai --with-source` through
  `cluster.sh ssh` and waits until the Kustomization is Ready at that revision; its new-cluster commits wait on the
  `yyc-ai.yml` run and the inventory as before (cutover 11.1).
- **The A1 gated ui rollout** (9.6) without the deploy job: (1) `08-ui-rollout` sets `spec.paused: true` on Deployment
  `jarvis/ui` together with the new template (Flux applies it; no pod changes); (2) once both old pods have had no
  running work for 10 min (E5), the runner suspends Kustomization `yyc-ai` (`flux suspend`, so a reconcile cannot
  pause or unpause behind the gate) and runs `cutover ui-gate` from the session: the same checks by exec, resume and
  re-pause by `kubectl patch` as 9.6 specifies; (3) when the rollout is complete, the runner pushes `09-ui-unpause`
  (git drops `paused`, which then equals the live object) and resumes the Kustomization; its reconcile changes
  nothing. Trailer `Deploy-Gated-Rollout` and the deploy job's gate hook go; the session's admin access to the old
  cluster replaces the deployer's `pods/exec` on ui pods. `09-` is one of the runner's own patches, so `freeze-check`
  passes it.
- **Cluster-level objects** YYC AI needs on the new cluster (namespaces `yyc-ai` PSS restricted, `yyc-ai-fabric` PSS
  privileged, `yyc-deploy`; namespaces `yyc` and `db` if T1 has not made them; the deployer's RBAC; the deploy
  connector; the JuiceFS CSI driver) live in the new cluster's cluster-level folder and are applied by the new
  cluster's own workflow (the Talos session's), never by `yyc-ai.yml`, for the reason of deploy-workflows 11. This
  design writes that folder `news-stream/deploy/clusters/talos/` and its database folder
  `news-stream/deploy/clusters/talos/db/`; the Talos session's own names hold if they differ.
- **Databases**: `magpie-pg` and `db/pg-shared` (15.5, 15.8) in the new cluster's database folder, applied by the
  same workflow; the roles and databases `hermit`, `egress`, `tools` stay in `news-stream/deploy/db/postgres.yaml`
  (old cluster, Flux `yyc-db`), next to Cluster `pg`.
- **Freeze**: `freeze-check` (in `yyc-ai.yml`'s `check` job, which now also watches `yyc-ai/legacy/**`) binds both
  folders as before; from A3 to A5 it also refuses commits to either cluster's node configuration (N8's hold is
  the runtime side).

### 15.4 Repository layout

- `yyc/deploy/`: the new stack only (platform 9.1's tree without `legacy/`), applied by `yyc-ai.yml` to the new
  cluster. A `check` step refuses any object in namespaces `jarvis`, `kube-system` or `flux-system` in its render.
- `yyc-ai/legacy/`: the old stack, today's `yyc/deploy/` files verbatim (with `sealed/`, `config.json`,
  `system-prompt.md`, `sealed-secrets-rbac.yaml`), applied by Flux on the old cluster. WP-39 moves it without
  touching a live object: `prune: false` on both Kustomizations; a commit adds the copy; a CI step shows
  `kustomize build yyc-ai/legacy` equal to `kustomize build yyc/deploy` byte for byte; `flux-sync.yaml`'s paths
  change to `./yyc-ai/legacy` and `./yyc-ai/legacy/sealed` (and its GitRepository include line), re-applied with
  `cluster.sh flux`; Flux's reconcile shows no change (ReplicaSet hashes and pod ages unchanged); a commit deletes the
  old copy, which Flux no longer reads; `prune: true` again. The window's old-side objects (the bridge Services and
  policies of 15.5, the importer's ServiceAccount, Role and token, `ui-to-pigeon`) are added here in A1. A6 deletes the
  folder except `legacy/catalogue/` while yyc-mcp still runs on the old cluster.
- Readers of `config.json` (tofu `access.tf`, `cloudflare.yml`'s path filter, `mcp-image.yml`, `catalogue.test.ts`,
  `yyc-ai-ami.yml`, `openrouter.py`, the ZDR guard's old half) name `yyc-ai/legacy/config.json` from WP-39's move until
  the switch commit moves them to `catalogue.json` (hermit 21.5); the ZDR guard's old half until A6.
- `yyc/yyc-catalogue` (read by yyc-mcp) is generated on the cluster that runs yyc-mcp: `yyc-ai/legacy/catalogue/`
  (its own kustomization in the old folder) reading `config.json` until the switch commit and `../../deploy/catalogue.json`
  from it (Flux's kustomize-controller builds with kustomize's load restrictor off; WP-31 checks this against the
  deployed v1.9.6 with `flux build kustomization yyc-ai`, and if it refuses, a copy kept byte-identical by a `check`
  step replaces the relative path). The ConfigMap's name, key and mount path never change. When yyc-mcp moves (T4,
  after A5), that move's commit adds the generator to `yyc/deploy/` (namespace `yyc` on the new cluster; the
  deployer's Role there) and deletes `legacy/catalogue/`.

### 15.5 Cross-cluster links (the bridges)

Both clusters are in Hetzner project `yyc` and its private network `yyc` (10.0.0.0/16, network zone eu-central),
during the move as the Talos plan states. Pod and Service addresses of one cluster mean nothing in the other, so each
call across goes to a fixed private address: a health-checked TCP proxy in front of a NodePort with
`externalTrafficPolicy: Local`, or, for Postgres, the existing NodePort `pg-hosts`. With `Local` the callee sees the
bridge's own address (the proxy connects from it; no SNAT inside the callee cluster), so its NetworkPolicy admits
exactly that /32 on that port, whatever the CNI; a node without a ready callee pod fails the health check and gets no traffic, so node reboots,
drains and T2's rebuilds need no change anywhere. On the caller's side a selector-less Service with one EndpointSlice
gives the address a cluster-local name, so no component's code or configuration names a cluster.

The two bridges, `yyc-bridge-old` (10.0.255.1, targets: the servers with Hetzner label `yyc-cluster=k3s`) and
`yyc-bridge-new` (10.0.255.2, targets `yyc-cluster=talos`), both minus servers that are no Kubernetes node (label
`role=nat`: yyc-nat1), live on one server, **`yyc-bridge1`** (`yyc/infra/bridges.sh up|check|down`, definitions
in `bridges.json`, Hetzner API with `YYC_HETZNER_API`, idempotent; WP-60). Not Hetzner load balancers: Hetzner
refuses to create one while the account's primary IP limit (16) is full, even with the public interface off at
create, and the limit stays (Deyao, 2026-11-05). yyc-bridge1 is a cax11 (ARM) in fsn1, Debian 13,
label `yyc-cluster=bridge`, with no public IP: network `yyc` is its only link (its own address from Hetzner's DHCP,
the two bridge addresses as Hetzner alias IPs), it reaches the internet (packages) through network `yyc`'s route
0.0.0.0/0 to yyc-nat1 and resolves through 10.0.0.30 (cloud-init), and SSH reaches it only through yyc-master1. It
runs HAProxy in TCP mode: per service one `listen` on the bridge's address and port, round robin to every target's
private IP at the NodePort, connections and health checks sourced from the bridge's address, a TCP health check
every 3 s with timeout 2 s, down after 3 failures (1 + 2 retries) and up after 2 passes, a failed connect retried on
another target; idle connections are kept for 1 h. The target list is read from the Hetzner API on every
`bridges.sh up`; a server that joins or leaves a cluster (T2's rebuilds, a new worker) needs an `up`, which
`bridges.sh check` reports (reboots and drains need nothing: the health check takes the node out). yyc-bridge1 is a
single server: while it is down, every link but B3 and B7 fails (the failure rows of cutover 13 and platform 25.3);
`bridges.sh down all && bridges.sh up` rebuilds it in minutes. Cost (Hetzner API `GET /v1/server_types`, 2026-11-05,
VAT 20 %): cax11 in fsn1 is EUR 5.99 net, EUR 7.188 gross a month, billed EUR 0.01152 gross an hour, no primary IP
(it replaces the approved EUR 16.776 of two lb11): from A1 until the later of A6 (B1, B2) and the T4 moves of the
monitor, yyc-mcp and the capture route (B4-B6); `bridges.sh down <link>` in the commit that removes a link, the server
going with the last one. Project `yyc` isolates the spend on Hetzner's invoice (EXPENSES.md's Hetzner line).

| # | Caller → callee | Callee side | Caller side | Each request authenticated by | Removed |
|---|---|---|---|---|---|
| B1 | pigeon (LU) and `magpie-import` (its LU reads of pending and preparing state) → old ui :8080 | old: Service `jarvis/ui-bridge` NodePort 30880 (`Local`) → `app=ui` 8080; `yyc-bridge-old` :8080 → 30880; NetworkPolicy `jarvis/ui-from-bridge`: 8080 from 10.0.255.1/32 (replaces `jarvis/ui-from-yyc-ai`) | new: Service `yyc-ai/ui-legacy` :8080, EndpointSlice 10.0.255.1:8080; pigeon's LU address is the constant `http://ui-legacy:8080`; egress rules of pigeon and `magpie-import` to 10.0.255.1/32:8080 | the person's Access JWT (the router verifies it, RS256 against the team JWKS) and, for LU, the pair token in `x-aviary-lu` | A6 |
| B2 | the old router's switch module (HTTP pass-on after the first move, 9.3) → pigeon :8080 | new: Service `yyc-ai/pigeon-bridge` NodePort 30890 (`Local`) → `app=pigeon` 8080; `yyc-bridge-new` :8080 → 30890; `yyc-ai/pigeon-from-bridge`: 8080 from 10.0.255.2/32 | old: Service `jarvis/pigeon-bridge` :8080, EndpointSlice 10.0.255.2:8080 (the switch module's pass-on target `http://pigeon-bridge:8080`); `jarvis/ui-to-pigeon`: egress to 10.0.255.2/32:8080 | the person's Access JWT, which pigeon verifies as at the edge | A6 |
| B3 | hermit, egress, tools, `magpie-import`, the import Jobs, the JuiceFS CSI mount pods, hermit's `juicefs` CLI and the new fabric's hosts → `pg` :5432 | old: NodePort `pg-hosts` 30432 (`Local`, on `yyc-pool-db-worker1` 10.0.0.2), unchanged; `pg-ingress` gains 5432 from 10.0.0.0/16 (sources are node addresses, N4); its 10.244.0.0/16 rules (the old stack's pods, the old fabric's JuiceFS mount pods) and 10.201.0.0/24 (the old fabric's hosts) stay, 10.201 removed at A6; the A1 commit's diff of `pg-ingress` is additive only (cutover 10.1 step 1) | new: Service `db/pg-shared` :5432, type NodePort 30433 (`Cluster`), EndpointSlice 10.0.0.2:30432; every database URL and JuiceFS `metaurl` of the new stack names `pg-shared.db.svc.cluster.local:5432`; the new hosts dial `<gw private IP>:30433` (15.6); namespace-wide egress in `yyc-ai` to 10.0.0.2/32:30432 | the role's password | T5 changes its target (15.8); the Service stays |
| B4 | the news-stream monitor → hermit :8080 (`/ops/snapshot`) | new: Service `yyc-ai/hermit-bridge` NodePort 30893 (`Local`) → `app=hermit` 8080; `yyc-bridge-new` :8083 → 30893; `hermit-in` admits 8080 from 10.0.255.2/32 | old: Service `yyc/hermit-bridge` :80, EndpointSlice 10.0.255.2:8083; `monitor.py`'s hermit URL in the switch commit (hermit 21.4) | `HERMIT_ACCEPT_MONITOR` | the monitor's move (T4) |
| B5 | the new tools → yyc-mcp :8080 | old: Service `yyc/yyc-mcp-bridge` NodePort 30881 (`Local`) → `app=yyc-mcp` 8080; `yyc-bridge-old` :8081 → 30881; `yyc-mcp-ingress` admits 8080 from 10.0.255.1/32 | new: placeholder Service `yyc/yyc-mcp` :80, EndpointSlice 10.0.255.1:8081, so `tools.json` keeps `http://yyc-mcp.yyc.svc.cluster.local/mcp`; tools' egress to 10.0.255.1/32:8081 | `Authorization: Bearer YYC_MCP_TOKEN` (tools.json headers) | yyc-mcp's move: its commit drops the placeholder from the inventory (`Deploy-Release`) and the real Service takes the name |
| B6 | the new tools → the web capture route | old: Service `yyc/web-bridge` NodePort 30882 (`Local`) → the capture pods; `yyc-bridge-old` :8082 → 30882; the capture route's ingress admits 10.0.255.1/32 | new: placeholder Service `yyc/web` :80, EndpointSlice 10.0.255.1:8082; tools' egress to 10.0.255.1/32:8082 | `WEB_FETCH_TOKEN` | the capture app's move, as B5 |
| B7 | `magpie-import` → the old Kubernetes API (10.0.0.3:6443): ui pods listed and watched with Terminating ones, `pods/exec` for the running sets (`holds.js`), `pods/proxy` for `/__jarvis/sockets` and `restart` per pod | old: ServiceAccount `jarvis/magpie-import`, Role `magpie-import-legacy` (`pods` get/list/watch, `pods/exec` create, `pods/proxy` get/create), RoleBinding, token Secret `jarvis/magpie-import-token` (type `kubernetes.io/service-account-token`), all in `yyc-ai/legacy/` | new: Secret `yyc-ai/magpie-import` key `MAGPIE_IMPORT_LEGACY_KUBE_TOKEN` (store `YYC_OLD_CLUSTER_IMPORT_TOKEN`), the old CA from `infra/yyc-cluster-ca.pem` (ConfigMap generator `legacy-kube-ca`), address binding `https://10.0.0.3:6443`; egress to 10.0.0.3/32:6443 | the ServiceAccount token | A6 |

- The switch module reads its bearer from header `x-jarvis-switch-token`, because the API server's pod proxy (B7)
  does not pass `Authorization` on to the pod. The runner's own calls (`move`, `admit`, the `/proc/net/tcp` checks,
  `restart` in a rollback) use the session's admin path to each cluster: `cluster.sh ssh` for the old one, N2 for the
  new one (pigeon's `/internal/switch/activity` through a port-forward).
- Nothing else crosses: magpie, `magpie-pg`, hermit's fabric and the egress VIP are all on the new cluster; the old
  egress and tools talk only to the old stack; `egress import-jarvis` reads `jarvis.egress_log` through B3.
- If the old API is down (a reboot of the single old master, which N10 rules out until A6 but which can still
  happen), B7's reads fail closed: releases wait (9.2), LU and the ui pods keep serving, the runner's `move` waits.
- Rehearsal **R6 (bridges)**, in A2 before R1: from each caller pod, a TCP connect and an authenticated request over
  each link; the same request without its credential refused by the callee; 20 LU sockets held 30 min through B1
  with only heartbeats (HAProxy's 1 h idle timeout) and none closed; a reboot of a new-cluster worker that
  runs a pigeon pod while B2 carries pass-on traffic: HAProxy drops that node within 11 s and no request on
  the other pod fails; a rolling update of pigeon while B2 carries pass-on traffic and one of hermit while B4 carries
  the monitor's reads: no pass-on or snapshot request fails; every NodePort probe runs with Talos's ingress firewall
  as built (N7); the hop's added latency p99 ≤ 2 ms (recorded, not a gate if higher: then stated to Deyao). For the
  rollouts, each bridge callee on the new cluster (pigeon, hermit) keeps serving for 12 s after it is marked
  terminating (a `preStop` sleep, longer than HAProxy's 3 checks × 3 s plus its 2 s timeout that take the
  node out), and kube-proxy sends `Local` traffic to a terminating, still-serving pod while its node has no ready one
  (terminating endpoints, Kubernetes ≥ 1.28).

### 15.6 The sandbox gateway on Talos

Talos has no shell, no SSH and no systemd units, so the gateway is a pod, as decision 13 states:

- DaemonSet `hermit-gateway` (namespace `yyc-ai-fabric`, `nodeSelector: {hermit.yyc/gateway: "true"}`, so exactly one
  pod on one node; `hostNetwork`, privileged, tolerations all, PriorityClass `serving`, RollingUpdate
  `maxUnavailable: 1`) runs **sandboxd in gateway mode as its main process** from `ghcr.io/de0ch/sandboxd:<sha12>`:
  `host.json` from ConfigMap `hermit-gateway`, the TLS files and `wg.key` from Secret `yyc-ai-fabric/hermit-gateway`,
  state and the image cache (the `:7444` image service for hosts launched before their AMI) on hostPath
  `/var/lib/hermit-gateway`, memory 256Mi request = limit (today's `MemoryMax`), readiness a TCP connect to
  10.202.0.1:7443. There is no `gateway.sh` installer, no `nsenter`, no host systemd unit.
- **The data plane lives in the host's network namespace, not in the process.** sandboxd in gateway mode leaves
  `wg0`, its nft table and its routes in place when it exits, and at start adopts what is there and reconciles it to
  `host.json` and its last peer set (never flush-and-recreate). A pod restart or a DaemonSet update therefore stops
  only the control API :7443 and the image service :7444 for seconds, exactly as a sandboxd restart does today
  (hermit 14.2); running sandboxes do not notice. WP-19 builds and tests this (kill -9 of the gateway process under
  traffic: no packet loss beyond the restart's seconds on :7443, `wg0` handshakes unchanged).
- Addresses: hermit 13.1 unchanged except the node: `privateGateway 10.0.0.1`, `controlCidr 10.0.0.0/16`,
  `podCidr` = `<pod-cidr>` (N4). The Hetzner network route 10.202.0.0/15 points at `<gw private IP>` (the gateway
  node's address in network `yyc`); `fabric/host/gateway-net.sh` sets it, the Hetzner firewall rule udp 51821 on that
  server and the node label, through the Hetzner and Kubernetes APIs only. The old fabric's route 10.200.0.0/15 →
  10.0.0.6 (`yyc-pool-apps-worker2`, old cluster) stays until A6; both routes live in network `yyc` and do not overlap.
- Firewall: N7 (the Hetzner Cloud Firewall filters only public interfaces, so private traffic needs no rule there).
- Overlay route: DaemonSet `hermit-overlay-route` on every new-cluster node attached to network `yyc` (nodeSelector on
  that label) adds `10.202.0.0/15 via 10.0.0.1` in the host namespace and sets `rto_min` in each ready egress pod's
  namespace, as before. A node outside the private network (another network zone, which the Talos plan allows) cannot
  reach the overlay; hermit and egress, the only pods that talk to 10.202.0.0/15 and 10.203.0.0/16, therefore run only
  on nodes in network `yyc` (any eu-central location, 15.7).
- **The path on Talos** (inputs I2, I3, I6): pod traffic between nodes rides Cilium's VXLAN inside KubeSpan, but the
  sandbox data path uses neither. Forward: the gateway sends what arrives on `wg0` to the pod network through
  sandboxd's table 51822 (`<pod-cidr> via 10.0.0.1`) over network `yyc`, where the cloud controller's per-node routes
  (I3) deliver it to the egress pod's node with the sandbox's source kept (the gateway's `keep_source` identity SNAT, as
  today). Reply: the egress pod answers within that connection (conntrack, so no masquerade applies), its node routes
  10.202.0.0/15 via 10.0.0.1 (the overlay route) and Hetzner's route 10.202.0.0/15 brings it to the gateway; the reply's
  pod source is routable in network `yyc` for the same reason as on the old cluster (a route for its /24). Because
  KubeSpan does not advertise pod networks (I6), no WireGuard peer sees a source outside its AllowedIPs. MTUs: the
  sandbox side's `wg0` is 1420, network `yyc` 1450, pods 1320 (I6); each TCP end already sends at most the smaller of
  its own MTU and the peer's MSS, and sandboxd's gateway table also clamps the MSS of SYNs forwarded from `wg0` to
  1280 (the pod MTU minus 40; WP-19), so no full-size packet meets a smaller hop whatever a sandbox advertises. sandboxd also sets
  `rp_filter` loose on the private interface (replies arrive there from pod addresses the node itself routes through
  Cilium). The WARP link is `warp0` and sandboxd's table 51822 and rule priorities 1000/1001 are used by neither KubeSpan
  nor Cilium (N7 checks the live tables).
- **JuiceFS metadata for the new hosts**: `<gw private IP>:30433`, the NodePort of `db/pg-shared` (B3). A host's
  packet reaches the gateway node over `wg0` (10.0.0.0/16 is routed to the hub), kube-proxy forwards it to `pg-hosts`
  and masquerades it, so the old cluster needs no route into 10.202.0.0/15 and `pg-ingress` sees the gateway node's
  own address. The address survives T5 (15.8) and the old fabric's `10.0.0.2:30432` is unchanged. If the chosen CNI's
  NodePort handling does not serve traffic that arrives on `wg0` (N7's test), sandboxd's VIP gets a second port
  instead: 10.202.1.1:5432, DNATed and masqueraded to `pg-shared`'s endpoints, which hermit pushes like the egress
  targets (13.8); WP-19 builds that only in that case.
- **What changes for hermit**: hermit 13.1's gateway row and route target; 14.2 (the gateway is a pod running
  sandboxd, not an installer); 14.7's runbook step 3 is the Talos upgrade or reboot of that node through the Talos
  session's upgrade workflow (N8), not `cluster.sh`; 21.1's API-server egress is a `CiliumNetworkPolicy` to the entity
  `kube-apiserver` (N3); the JuiceFS URLs name `pg-shared`; hermit pods require nodes in network `yyc`. Nothing else in
  hermit changes; sandboxd gains leave-on-exit and adopt-on-start for gateway mode, the MSS clamp and `rp_filter`
  loose on the private interface. The single gateway node remains the stated gap of
  7.3 item 1.

### 15.7 Placement and sizes on the new cluster

- Every YYC AI pod on the new cluster (`yyc-ai`, `yyc-ai-fabric`, `db/magpie-pg`) requires a node attached to network
  `yyc` (`nodeAffinity` on N5's label `yyc.hetzner/network=yyc`), in any location of its network zone eu-central
  (fsn1, nbg1, hel1 all attach to its one subnet): `pg-hosts`, the bridges (which target nodes by
  private IP), the old API and the overlay route are all reached over network `yyc`. Everything else prefers fsn1
  (a preferred affinity on `topology.kubernetes.io/region`, weight 50), where `pg` and the old stack are.
- **Egress leaves through yyc-nat1 in fsn1** (change of 2026-11-05, Deyao: no raise of Hetzner's primary IP limit, so
  the workers have no public IP; cluster README "Workers without public IPs"): the workers have no WARP of their own and
  no IPv6; network `yyc`'s route 0.0.0.0/0 takes their outgoing traffic to yyc-nat1 (10.0.0.30, fsn1), which leaves
  through its WARP. egress's internet traffic goes by name through the socks pool (`egress/socks`, one copy per worker,
  cluster README "Outgoing proxy pool"), so every pod's exit is that one WARP address whatever its node's location
  (104.28.207.215, the old cluster's exit too, both read 2026-11-05). So egress needs no location (network `yyc` only),
  N5 no fsn1 workers, and "two workers in fsn1" no longer applies. The gateway node is in fsn1 (N7) for the AWS hosts'
  latency, not for an exit. A single yyc-nat1 is a single point of the new cluster's way out: while it is down the
  sandboxes reach nothing outside (the cluster README states it; Claude's reading of the consequence for YYC AI).
- **`magpie-pg`'s three instances sit in one location** (a required affinity to the location chosen at A1: fsn1 unless
  N12's room is elsewhere), because a Hetzner Volume stays in its location and a failover or drain moves an instance
  only within it. Its volumes are new, so any location works; StorageClass `yyc-block` binds `WaitForFirstConsumer`,
  so each volume is created in its pod's location and one StorageClass serves every location.
- Every two-replica Deployment spreads by hostname with `whenUnsatisfiable: DoNotSchedule` (maxSkew 1): the new
  cluster's scheduler packs nodes (MostAllocated), and a preferred spread could put both replicas on one node.
  `magpie-pg` keeps its required anti-affinity across three nodes.
- Requests during the window (platform 16.1, memory request = limit): pigeon 2 × 384 Mi, magpie 2 × 1536 Mi, hermit
  2 × 384 Mi, egress 2 × 256 Mi, tools 2 × 128 Mi, connectors of tunnel `pigeon` 2 × 64 Mi, JuiceFS mount pods 4 ×
  192 Mi (hermit's two and the importer's two), `magpie-pg` 3 × 1152 Mi (1 Gi plus its 128 Mi sidecar), `magpie-import`
  2 × 1024 Mi, the gateway 256 Mi, the overlay route 32 Mi per node (4 nodes), the deploy connector 2 × 64 Mi:
  12 288 Mi, plus 1 536 Mi of rollout surge (magpie's), **13 824 Mi (13.5 GiB)** at A3-P4, on at least three nodes in
  network `yyc`. After A6 (no importer and its two mount pods): 9 856 Mi plus the surge. The cluster add-ons (CNPG, Sealed
  Secrets, the CSI drivers, the cloud controller, Alloy) are the Talos plan's own.
- On the old cluster nothing new lands: the old stack (about 4.3 GiB) stays, plus A1's two 1.5 GiB surge pods; at A5
  it scales to 0 but its room stays until A6 (N10).
- **Room side by side (N10, N12)**, read 2026-11-03: the old cluster has 43 412 Mi allocatable (4 × 6.5 GiB + 6 ×
  2.7 GiB, master included) with 26 684 Mi requested; its peak in the window is 31 804 Mi (N10). The new side's peak is
  13 824 Mi plus about 600 Mi of each node's own pods, about 16 200 Mi on four nodes. Together about 48 000 Mi, more
  than every existing server holds, so T2's rebuilds of old servers cannot supply the new side: N10 leaves T2 about
  three cx23 (2 770 Mi allocatable each on k3s), and a node there holding a magpie pod and a `magpie-pg` instance
  needs 2 688 Mi before its share of the rest. The window therefore gets **three new cpx32 in fsn1** (4 vCPU, 8 GB,
  x86), bought in WP-59 in project `yyc`: Hetzner API `/v1/pricing`, 2026-11-03, EUR 42.588 gross a month each
  (EUR 0.06828 an hour), EUR 127.764 a month for the three (EUR 0.20484 an hour). If Talos reserves what k3s reserves
  on an 8 GB server (6 701 Mi allocatable), the three give 20 103 Mi for about 15 600 Mi (Claude's estimate). Why
  this type: every YYC AI image is built for linux/amd64 only (no workflow in DE0CH/YYC builds `linux/arm64`,
  2026-11-03), so cax21 (8 GB ARM, EUR 12.588, fsn1) would first need multi-arch builds of every image; cx23 and cx33
  are on sale nowhere; cpx22 (4 GB, EUR 23.388) costs more per GiB and fits fewer magpie pods. The Talos plan's
  temporary cx23 in hel1 is not on sale and could not run egress. After A6 the three stay as workers until T0's
  packing plan (the builder's, which gets both peaks as inputs) decides which servers the new cluster keeps.

### 15.8 Databases

- **`magpie-pg`** (decision 2): on the new cluster in namespace `db`, exactly pg-ha 4 (three synchronous instances,
  `failoverQuorum`, `pg`'s image, `pool=apps` in one location of network `yyc` (15.7), priority `serving`),
  `magpie-pg-ingress` admitting 5432 and the instance status port 8000 by selector only: the `magpie-pg` instances
  themselves (`cnpg.io/cluster: magpie-pg`, streaming among them), the CloudNativePG operator's pods (its namespace, the
  builder's), the dump CronJob's pods `app=pg-dump-magpie` in `db`, and in namespace `yyc-ai` the pods `app=magpie` and `app=import`; never an address (under Cilium an
  `ipBlock` does not match pod addresses, N3) and never the sandbox hosts; its nightly base backup to `pg-pitr` under server name `magpie-pg`; a daily logical dump CronJob
  `pg-dump-magpie` (new cluster, database folder) into the same Object Lock bucket and prefix scheme as
  `pg-dump-daily`. Applied by the new cluster's workflow (15.3).
- **`hermit`, `egress`, `tools`**: databases on Cluster `pg` on the old cluster, as designed (5.3, 5.4), reached
  through B3 (`pg-shared.db:5432`); their roles and `Database` CRs in `postgres.yaml` (Flux `yyc-db`); dumped by the
  old `pg-dump-daily`. They are not put in `magpie-pg`, which decision 2 makes magpie's own, and the JuiceFS metadata
  must stay on `pg` while the old fabric still mounts the volume. They move with `pg` in T5: the replica cluster
  copies every database.
- The importer's sources `jarvis` and `cloudcli` are read-only connections through B3, as before.
- **T5 (after A6, and after T4 has moved every app that writes to `pg`, N11)**: the builder's CloudNativePG replica
  cluster `pg` on the new cluster follows the old primary. At promotion (writes pause up to a minute, its 04:00 HKT
  window) one commit to the new database folder changes Service `db/pg-shared`: `selector: {cnpg.io/cluster: pg,
  cnpg.io/instanceRole: primary}` and `targetPort: 5432` (was 30432, `pg-hosts`' NodePort), and the same commit deletes
  the manual EndpointSlice `db/pg-shared-manual` (labels `kubernetes.io/service-name: pg-shared`,
  `endpointslice.kubernetes.io/managed-by: yyc-manual`). The EndpointSlice controller manages only its own
  slices, so a selector alone would leave the manual one and kube-proxy would spread connections over the demoted old
  primary (read-only) and the new one. The builder's workflow prunes that object (its prune trailer) or, if it does
  not prune, runs `kubectl -n db delete endpointslice pg-shared-manual` in the same run. A `yyc-ai.yml` commit before
  T5 adds to `postgres-shared` an egress rule to the new `pg` pods by selector (namespace `db`, `cnpg.io/cluster: pg`,
  5432), and the new `pg`'s ingress admits hermit, egress, tools and the importers by selector and the gateway node's
  NodePort traffic (masqueraded to the node: Cilium entities `host` and `remote-node`). Check right after the commit:
  `kubectl -n db get endpointslices -l kubernetes.io/service-name=pg-shared` lists exactly one ready address, the new
  primary's pod on 5432, and a write through `pg-shared` from a hermit pod and a JuiceFS metadata write from a host
  succeed. Clients reconnect when the old primary is demoted; no URL, Secret, host or AMI changes. `/workspace` I/O of running sandboxes waits and retries for that minute (the stated gap 7.3 item 2).

### 15.9 Sealed Secrets on two clusters

The new cluster's controller has its own key; its public cert is `infra/sealed-secrets-talos.pem` (N6).
`infra/seal.py` picks the cert by the sealed file's folder: `yyc/deploy/sealed/` and
`news-stream/deploy/clusters/talos/**` get the new cert, every other folder (`yyc-ai/legacy/sealed/`,
`news-stream/deploy/apps/sealed/` and the rest) the old `infra/sealed-secrets.pem`; `--check` refuses a sealed file
whose recorded cert does not match its folder. Every new-stack Secret of platform 12.2 is sealed for the new cluster
only. The old-cluster Secrets the switch touches are sealed for the old one: `jarvis/ui-env` (A1's two tokens), the role passwords `db/pg-role-hermit|egress|tools`. One new store key:
`YYC_OLD_CLUSTER_IMPORT_TOKEN` (B7, read once from the old cluster's token Secret into a file and written with
`scripts/store-put.py`, never printed). The Talos session re-seals the other apps' Secrets for T4 itself; the two
sessions never seal the same file.

### 15.10 Cloudflare tunnels

- `yyc-ai` (connectors `jarvis/cloudflared`, old cluster): unchanged until `20-old-connectors-0` (A3); tofu
  teardown in A6.
- `pigeon` (connectors `yyc-ai/cloudflared`, new cluster) with `pigeon-test`: A1, as platform 14.1. The DNS flip of
  `ai.yyc-internal.com` moves staff's new connections between clusters.
- `yyc-deploy` (connector `yyc-deploy/cloudflared`, new cluster only; the old cluster never gets it): tofu `deploy.tf`
  is turned on in WP-59, ingress `tcp://kubernetes.default.svc.cluster.local:443` on the new cluster; the Access app,
  service token and GitHub secrets are as deploy-workflows 3 and 7.
- `ai-relay` (WP-58, if the relay is kept): routes to the new egress on the new cluster.
- The other YYC tunnels move with their apps in T4 (the Talos plan: each connector moves with its app, so its address
  does not change).

### 15.11 Rollback across clusters

The old stack keeps its whole cluster footing until A6 (N10), so 9.10 holds unchanged, with these mechanics:

| When | What crosses clusters |
|---|---|
| A0-A2 | new-cluster commits are reverted through `yyc-ai.yml` (with `Deploy-Prune` trailers), old-cluster commits through Flux; `bridges.sh down` only if the project is abandoned |
| A3, before the gate | `rollback flip` as 9.10: creates through LU (B1), reverse hand-over of pigeon-born conversations, then the DNS revert sends new connections back to tunnel `yyc-ai` on the old cluster, and `admit` (through the old cluster's admin path) stops the pass-on (B2) |
| A3-A5, after releases | per conversation behind pigeon (new cluster): the reverse export writes into JuiceFS `users/` (the shared volume) and `import_sources`; `restart` reaches the person's old child through B7; LU (B1) serves it afterwards |
| P4 | `rollback old-up`: Flux reverts `40-` and `30-` on the old cluster, whose room N10 kept; then per conversation as above |
| after A6 | no rollback to the old stack (unchanged) |
| the new cluster lost as a whole | before the gate, `rollback flip` needs only Cloudflare and the old cluster. After releases it is a **stated gap**: owned conversations can be neither served (pigeon, magpie) nor reverse-exported (that needs magpie) until the new cluster is back, and their state is in `magpie-pg` there. Restore target: the new cluster rebuilt by the builder's runbook (its hourly etcd snapshot in Object Storage, or a fresh Talos cluster from the machine configs in git), then `magpie-pg` recovered from `pg-pitr` (CNPG `bootstrap.recovery` from the nightly base backup plus WAL, server name `magpie-pg`) by the database folder's workflow, then a `yyc-ai.yml` run; runbook: cutover 16's row "the new cluster lost as a whole". Not designed: an emergency magpie and pigeon on the old cluster from the same images, because after A5 the old side's kept room (the old stack's 4.3 GiB plus the 3 GiB surge slots) holds `magpie-pg`, magpie and pigeon alone (about 7.3 GiB) with nothing left for hermit, egress and a gateway, without which no turn runs. Before decision 13 one cluster held both stacks, so this case was the loss of that one cluster |

### 15.12 What decision 13 removes or replaces

- **WP-39, the Flux hand-over** (deploy-workflows 8, H0-H4): dropped. Flux keeps the old cluster until T6; the old
  master is never restarted for an authenticator. `yyc-apply`'s `seed`, `snapshot` and `adopt`, `yyc-ai.yml`'s
  `HANDOVER_EXCLUDE`, `ACCESS_PENDING` and `jarvis` namespace, `master-authn.sh` and its `hetzner-k3s.yaml` line,
  and `cluster.sh api-forward` are deleted (nothing uses them). WP-39's number now names the `legacy/` folder move,
  which, like WP-40, runs only after T1-T2 (decision 13: "WP-39/40 onward").
- **Trailer `Deploy-Gated-Rollout` and the deploy job's `ui-gate` hook**: replaced by the runner's gate with Flux
  suspended (15.3).
- **`yyc-pool-small-worker1` (10.0.0.5) as the gateway node**, `gateway.sh`'s host install and `cluster.sh`'s gateway
  guard: replaced (15.6).
- **`pg-ingress` admitting 10.202.0.0/24** and the `pg-ingress` exclusion of `magpie-pg`'s pods on the old cluster:
  replaced by an addition: `pg-ingress` gains 5432 from 10.0.0.0/16, and its 10.244.0.0/16 rules and 10.201.0.0/24
  stay (the old stack's pods and the old fabric's JuiceFS metadata until A6; 10.201 removed at A6), so the A1 commit
  only adds; `magpie-pg` is not on that cluster.
- **The old stack's cluster-scoped and `kube-system` objects in `yyc-ai.yml`'s render** (PV `yyc-ai-jfs`, RuntimeClass
  `gvisor`, `sbx-overlay-route`, `yyc/yyc-catalogue`): never there; they stay in `yyc-ai/legacy/` and leave through
  Flux's prune in A6 (the catalogue generator later, 15.4).
- **`clusters/hetzner/{yyc-ai-access,deploy-access,juicefs-csi}.yaml`** (written for the old cluster, never listed in
  its kustomization): moved to `clusters/talos/` and rewritten for the new cluster (deploy-workflows 12).
- **New recurring cost**: the bridge server yyc-bridge1, EUR 7.188 gross a month for the time it exists (15.5); three
  cpx32 in fsn1 for the new side's room, EUR 127.764 gross a month (EUR 0.20484 an hour) from WP-59 until T0's packing
  plan replaces them (15.7, N12); the `magpie-pg` volumes stay EUR 2.0592 a month; the rest of the new cluster's
  servers are the Talos plan's. All in project `yyc`, so Hetzner's invoice isolates them; EXPENSES.md's Hetzner line
  gains the bridge server (WP-60) and the three servers (WP-59) in the commits that create them.

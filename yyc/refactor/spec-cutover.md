# The switch-over: from ui / web / CloudCLI to pigeon / magpie / hermit / supervisor (spec)

Status: implementation spec, 2026-11-02, revised after review round 1 (section 21). Inputs:
`magpie-target-design.md` (authoritative), `refactor/inventory.json`, gap reports 0-3, the four component specs
(`spec-pigeon.md`, `spec-magpie.md`, `spec-hermit.md`, `spec-supervisor.md`) and the platform spec
(`spec-platform.md`). Live numbers below were read on 2026-11-02 from the cluster (read-only `kubectl`, a `begin
transaction read only` on database `jarvis`).

**Revision 2026-11-02 (Deyao's decisions, `DECISIONS.md`).** The switch-over's text of record is DESIGN.md section 9
(decision 9: the switch runs whenever it is ready, no time window; decision 11: brief reconnects allowed, nothing read-only, cut or lost; decision 12: no
R5), with the mechanisms of `switch-final.md` it cites (the legacy upstream's contract, tickets on an outcome, fence
and release, upload ingestion, reverse-export completeness); switch-final's observer, move rule, person states, seq
remap, in-place router upgrade and 72 h tab wait are gone (DESIGN 9.11). Phases A0-A7. This spec still owns the
importer's sync code, byte-exact verification and release transaction (7), the one-off copies (8), the front door's
tofu (9), the P0-P2 work (10.0-10.2), the P4 checklist (10.4), the deletion steps (10.5-10.7, 15) and the reverse
export (14.3). Sections 2-6, 7.1-7.5, 7.8-7.12, 8.2-8.3, 9.2, 10, 11-14, 16 and 20 were rewritten to match DESIGN 9;
where any text here still disagrees with it, DESIGN 9 wins. Decision 1: `yyc/deploy` is applied by workflow
`yyc-ai.yml` (`deploy-workflows.md`), not by Flux. Decisions 5 and 6 settle 15.4 and 7.4.2. Decision 10: Cluster
`pg` stays one instance (1.1, 1.2, 10.0). Phase names: P0 = A0, P1 = A1 (with the one ordinary ui rollout carrying
the router's switch module, DESIGN 9.6), P2 = A2, P3 = A3-A5, P4 = P4 (after A5), P5 = A6, P6 = P6, and A7 (the
legacy-path cleanup, 10.7).

**Revision 2026-11-03 (decision 13; text of record DESIGN 15).** The new stack deploys onto the new Talos cluster and
the old stack stays on the old k3s cluster, so the switch also moves YYC AI between clusters. Changed here: 5.1 (the
two stacks on two clusters), 7.2 (the importer's credentials and paths across clusters), 10.0 (P0's live work: the
`legacy/` folder move instead of the Flux hand-over, and the new cluster's footing after the Talos session's steps
1-2), 10.1 (P1's commit order, with the bridges), 11.1-11.2 (the runner on two clusters, Flux for old-side commits,
the A1 gate with Flux suspended), 14.1 and 16 (rows across clusters); section 23 collects the rest. Where any other
text here still describes one cluster, DESIGN 15 holds.

This spec, with DESIGN 9, is the plan for the switch. Where it differs from a component spec's own cut-over section
(magpie 19, pigeon 20, hermit 22.3-22.5) or from the platform spec's switch view (platform 18), this spec wins and
section 17 lists the edits those specs need. Section 21 records where each review point of round 1 landed, section 22
the points not taken and why.

Contents
1. Starting point and end state
2. How the rules are applied (what "clean cut, no dual reads" means here)
3. Decisions taken in this spec
4. Live numbers that size the plan
5. Two stacks side by side: topology and what runs when
6. Who owns a conversation: the ownership state machine
7. The importer (`magpie import`): one bulk import, then a per-conversation handover
8. The other one-off copies (hermit, egress, tools)
9. The front door: a second tunnel (`pigeon`) and a one-line DNS flip
10. Phases, with commits, verification checklists and "done"
11. The switch day (the `cutover` runner)
12. Running chats and open tabs during the window
13. Version skew: iOS builds and web tabs
14. Rollback
15. What gets deleted, and when
16. Failure modes
17. Changes the component specs need
18. Inconsistencies between the component specs noticed while writing this
19. Coverage of the gap-report items and inventory features this spec owns
20. Open questions for Deyao
21. Review round 1: where each point landed
22. Rejected review points
23. Two clusters (decision 13)

---

## 1. Starting point and end state

### 1.1 Today (live, 2026-11-02)

| Piece | Where | Notes |
|---|---|---|
| Front door | tunnel `yyc-ai` (tofu `cloudflared.yyc_ai`), DNS `ai.yyc-internal.com` proxied CNAME to it, Access app `staff_ai` (paths `api, paseo-ws, ws, ops, __jarvis` on hosts `ai.yyc-internal.com`, `ai-yyc-ui.deyaochen.com`) | the 301 for `ai.yyc.deyaochen.com` is `yyc-edge`'s `MOVED` map (stays) |
| Connectors | Deployment `cloudflared` in ns `jarvis`, 2 pods, `--grace-period 3m`, grace 200 s | from the jarvis3 base |
| `ui` | Deployment `ui` (2 pods, `ghcr.io/de0ch/yyc-ai-ui:3de3f5d8771b`): router.mjs + one CloudCLI child per signed-in person (max 4 per pod), 900 s drain | turns live in the child; the child holds the `/proc` socket to the sandbox |
| `web` | Deployment `web` (2 pods, `jarvis3-chat:3f7499a9ddd0`): `/internal/cc/*`, scaler, janitor, ops page | old fabric: gateway `yyc-sbx-gw` on `yyc-pool-apps-worker2`, 10.200/15, EC2 tag `app=yyc-ai-sandbox` |
| `egress`, `tools` | Deployments in `jarvis`, both on database `jarvis` (`egress_log`, `tool_calls`, `kv`, `sandboxes` lookups) | egress announces itself in `kv egress_pod:*` |
| Conversation state | CloudCLI's `cc_u<key>` schemas in database `cloudcli` (sessions, failed turns), `cc_shared.shares`; Claude Code's JSONL under `/jfs/users/<key>/claude/projects/-workspace/`; sent files `/jfs/sent/<conv>/`; shares `/jfs/users/<key>/shares/<id>/`; workspaces `/jfs/ws/<conv>` | JuiceFS volume `yyc-ai`, metadata in `jfs_yyc_ai` |
| GitOps | Flux Kustomizations `yyc-ai` (`./yyc-ai/deploy`, prune) and `yyc-ai-secrets` (`./yyc-ai/deploy/sealed`, fenced applier); tofu by the `cloudflare.yml` workflow (drift revert every 30 min) | `yyc-db` (`prune: false`) owns databases and roles; from A0 `yyc/deploy` is applied by `yyc-ai.yml` instead (deploy-workflows 8) |
| Postgres | Cluster `pg`, one instance | stays one instance (decision 10) |

### 1.2 End state

| Piece | Where |
|---|---|
| Front door | tunnel `pigeon` (tofu `cloudflare_zero_trust_tunnel_cloudflared.pigeon`, platform 14.1), the same DNS name and Access app; signed-in paths `api, paseo-ws, ops` |
| Namespaces | `yyc-ai`: `pigeon`, `magpie`, `hermit`, `egress`, `tools`, `cloudflared` (connectors of tunnel `pigeon`); `yyc-ai-fabric` (PSS privileged): the `hermit-gateway` and `hermit-overlay-route` DaemonSets (platform R24) |
| Fabric | hermit's: gateway DaemonSet `hermit-gateway` (ns `yyc-ai-fabric`, the new cluster's gateway node, DESIGN 15.6), 10.202/15, EC2 tag `app=hermit-host` |
| Databases | `magpie` on Cluster `magpie-pg` (3 instances, synchronous); `hermit`, `egress`, `tools` (plus `jfs_yyc_ai`, unchanged) on Cluster `pg`, one instance as today (decisions 2 and 10); `jarvis` and `cloudcli` dropped, the old chat UI's data archived (decision 5) |
| GitOps | `yyc/deploy` applied by workflow `yyc-ai.yml` (server-side apply, prune by trailer, waits for rollouts); tofu by `cloudflare.yml`; Flux keeps `yyc-apps`, `yyc-db` (prune false), `yyc-aiwatch` |
| Storage | bucket `yyc-ai-magpie` (blobs), JuiceFS `ws/` only (`users/` and `sent/` removed 30 days after the switch) |
| Code | aviary: pigeon, magpie, hermit, supervisor, sandboxd; jarvis3: egress, tools; claudecodeui: the Paseo apps (and whatever Jarvis 4's local mode keeps, which stays open); YYC: `yyc/deploy` with only the new stack (platform 9.1 layout), `catalogue.json` the one access catalogue of every YYC app |

Nothing named `ui`, `web`, `jarvis` (as a YYC AI namespace or database), `cloudcli` or `JARVIS_*` remains in YYC AI's
cluster objects, databases, secrets or code paths after phase A6 (section 15).

---

## 2. How the rules are applied

**One writer per conversation, at every moment.** The design forbids dual reads: no service ever reads a
conversation from two stores, and no service reads the old store at all. What this spec allows instead is an
ownership transfer: every conversation starts owned by the old system; magpie holds a copy that only the importer
writes; at a moment when the old system provably can no longer write that conversation, the importer makes a last
copy, checks it byte for byte and hands the conversation to magpie for good. From then on the importer never touches
it and the old copy is dead data. The column that marks the state (`conversations.migrating`) and the importer are
deleted in A6. This is a clean cut per conversation, made at the safest instant for each one (when it is idle and
the old path is gone, so it is reachable only through pigeon), rather than one global instant (which would need either downtime or a reverse
sync).

**"Imported once."** Each conversation is imported once, in one bulk run in A2 (`magpie import run`). When its
hand-over comes (A4), the same sync code runs one last time for that conversation: it appends what the old system
wrote since the bulk run, or applies magpie's suffix rewrite (magpie 6.6) where Claude Code rewrote a file
(compaction, tombstone removal), verifies byte for byte and releases. The only repeated passes are in the window
(A3 to the end of P4), where the hand-over loop re-reads legacy conversations every 5 s and at once on a ticket's
NOTIFY: it keeps each idle conversation pre-verified so a release only re-stats, and it watches each one's release
condition (DESIGN 9.4). A tab reaches a legacy conversation through pigeon's legacy upstream (LU), served by the same
old child as before the switch; once released it is served by magpie.

**Two complete stacks, no shared component.** The old stack (ui, web, its egress, tools, connectors, fabric) keeps
running unchanged in namespace `jarvis` until it is drained. The new stack runs in a new namespace `yyc-ai` with its
own egress, tools, connectors, tunnel and fabric. No component of either stack reads the other's database or speaks
the other's protocol. The links between them are only: the importer (one way, old to new), the DNS record (which
tunnel the name points at), and the shared JuiceFS volume, where each `ws/<conversation>` is used by exactly one
stack at a time because a conversation's sandbox can only be started by the stack that owns the conversation. For
the window pigeon's legacy upstream and the old router's switch module are added links (DESIGN 9.1); both are
deleted in A6.

**No deployment switches.** The new components get no "migration mode": a legacy-owned conversation is a row state
only the importer can set; pigeon routes its writes through its legacy upstream under a ticket and magpie's fence
makes a native write wait at most 3 s (6.4, 6.5). All of it is deleted in A6, the legacy-path set in A7.

**Zero downtime; decision 11's reconnects only.** No step stops serving. The new stack serves before the name points
at it; at T0 the DNS moves; right after the settle time every open old tab is moved (a request-free close, one ~2 s
reconnect, the tab resumes on pigeon with its cursors valid); then the old connectors go to 0 and conversations are
handed over as they are idle (most at once, the rest after their old turn ends); running turns finish where they
started; the old stack retires last (DESIGN 9.2-9.4). Every rollout uses the components' own surge-first rollouts.
The importer's hand-over Deployment has two replicas and a leader lock like any singleton (7.1).

**What may never happen (decision 11).** No conversation is read-only at any instant, no running turn is cut or
lost, no input is lost, no attachment fails and no ask is lost. Allowed: one ordinary ui rollout in A1 (gated on
no running work on the old pods, DESIGN 9.6; every open tab reconnects once), the move at T0, one repaint of a shown conversation at its release, one UI refresh on a rollback.
The proof is DESIGN 9.9's zero counters and persona rig.

---

## 3. Decisions taken in this spec

| # | Decision | Why |
|---|---|---|
| D1 | The new stack lives in a **new namespace `yyc-ai`**; the old stack stays untouched in `jarvis` and is removed by deleting that namespace (hermit 1.1 said "the namespace stays `jarvis`": changed here) | egress, tools and cloudflared keep their names but must run in an old and a new version at once from A3 until A5 (a few hours), and stay present at 0 until P4 ends (old sandboxes need the old egress's token lookups and `kv` announcements; new sandboxes need the new egress's fixed names). Two Deployments with the same name need two namespaces. It also makes the final delete one object |
| D2 | A **second tunnel `pigeon`** (platform 14.1's names) with its own connectors in `yyc-ai`; the switch is one DNS change of `origin_ai` from tunnel `yyc-ai` to tunnel `pigeon` | no NetworkPolicy has to cross namespaces; both tunnels carry identical hostname rules from P1 on, so the DNS record alone decides (no ingress change races the DNS change); the rollback is the same one line; once every old tab is moved, the old connectors go to 0 and no old path is left, without touching anything the new stack uses |
| D3 | **Per-conversation ownership** with `conversations.migrating`; in magpie the importer is the only writer of a migrating conversation, while the old stack writes it (old-path sockets, or pigeon's LU under a ticket); **release per conversation** only when it is idle and the old path is gone (6.3, DESIGN 9.4) | one writer at every instant, and no conversation is ever refused, frozen or cut |
| D4 | The importer is the **`magpie import` subcommand family of the magpie image** (platform R15, magpie 19): P2's bulk import and verify run as Jobs; the hand-over runs as Deployment `magpie-import` (2 replicas, `magpie import handover`, leader by advisory lock, plus the writer sidecar of 14.3) from `start-handover` until P4 ends (switch-final C14.3), then the Deployment is deleted (`import-done`) | Jobs for one-shot work; the hand-over needs a process that watches for hours and must survive a node drain, which is the singleton-with-leader-lock shape the zero-downtime rule asks for |
| D5 | The switch **runs whenever it is ready; no time window** (decision 9; Deyao 2026-11-06: working hours were allowed, never required) | nothing staff do is lost or blocked, so no quiet hour or working-hours window is needed; Claude watches the runner live |
| D6 | Shares' frozen workspaces are **cloned** (`juicefs clone`, metadata only) to `ws/<uuidv5(share)>`, not renamed (hermit 22.3 said rename) | a rename before the switch breaks the old system's share viewer, which still reads `users/<key>/shares/<id>/ws` until A5 |
| D7 | egress and tools each get their **own database** (`egress`, `tools`); their history is copied once and `egress_log` is copied every 60 s until the old egress is gone; nothing is copied back (8.3) | "each owns its database"; the budget is a sum over `egress_log`, so the new egress needs the old stack's spend; the window is a few hours, so the old egress's not seeing new spend is covered by preflight headroom |
| D8 | A native mutation of a **fenced** conversation waits for the release (≤ 3 s); of an unfenced legacy one, magpie answers **409 `{"code": "conversation_migrating", "route": "legacy"}`**, which pigeon turns into an LU write at once (its owner cache was stale) | no refusal ever reaches a tab (decision 11: nothing read-only) |
| D9 | A running old turn **finishes on the old stack**, live in every tab (old path until the move, then LU), with Stop working; no import-origin turn is synthesised and nothing is cut (DESIGN 9.5) | with the old stack reachable behind pigeon, letting a turn finish where it started costs nobody anything |
| D10 | **A conversation is released only after its old sandbox is `gone`**, by the old janitor's own idle release; the switch never releases an old sandbox (DESIGN 9.4) | a Claude Code process whose `/proc` FIN was lost could otherwise write to the old transcript after hand-over; releasing a warm sandbox early would make the person's next turn slower |
| D11 | **P4, no wait** (Deyao, 2026-11-07: "Stop waiting"; the 72 h soak was the plan's own margin, dropped): after A5 the old stack stays scaled to zero but present and the bridge kept (import Deployment, NetworkPolicies, the egress copy) until P4's checks pass, then `import-done`; rollback is the per-conversation reverse hand-over (14.2) with the reverse export (14.3) of the conversations that changed on the new side; the others need nothing (their old files are untouched, `verify --all` proves it) | a rollback must not lose turns staff made on the new stack (section 22, point 2) |
| D12 | The `cutover` runner lives in the YYC repo (`yyc-ai/cutover/`, Rust, built and run in the operating session and by the `check` job for `freeze-check`; no image), the importer in aviary (library crate `crates/magpie-import`, linked into the `magpie` binary as `magpie import …`); both are deleted in A6 | the runner is YYC operations; the importer must write rows exactly as magpie does |
| D13 | The first `pigeon-app` bundle is **seeded with the chunks of every ui image served within the 720 h Access session lifetime** (`/app/dist/_expo/static/**` of each, DESIGN 9.6), and ui and `pigeon-app` stay at the A1 claudecodeui commit until A5 | web tabs loaded from any ui build keep fetching their own lazy chunks from pigeon after they reconnect |
| D14 | `ai-yyc-ui.deyaochen.com` stays on both tunnels until A6, then goes (tofu, the hand-set CNAME in Deyao's zone, `monitor.py` `SAAS_NAMES`) | it is an origin alias of `origin_ai`; dropping it earlier would make it 404 at the flip for no gain |
| D15 | **One switch commit** (`patches/10-flip.patch`, one YYC push): the tofu DNS flip (platform 14.2), every catalogue reader moved to `catalogue.json` (hermit 21.5), the news-stream monitor on hermit's snapshot (hermit 21.4); the signed-in path shrink (`ws`, `__jarvis`) waits for A6, since old pages may exist until A5 | hermit 21.4/21.5 and platform 18.2 call this "the cut commit"; one commit is one revert |
| D16 | The ConfigMap `yyc/yyc-catalogue` **keeps its key `config.json`**, sourced from the new file (`files: [config.json=catalogue.json]`), so yyc-mcp, CronJob `mcp-cleanup` and their `CATALOGUE=/etc/yyc-catalogue/config.json` never change | renaming the key would delete the file from running yyc-mcp Pods before Kustomization `yyc-apps` could roll them (A7.F3, A19.F12) |
| D17 | `config.json` and `system-prompt.md` **stay at `yyc/deploy/`** (with the two generators that read them) until A6 (10.5 step 4); only the old stack's other files move into `legacy/` | tofu, `cloudflare.yml`, `mcp-image.yml`, `catalogue.test.ts`, `yyc-ai-ami.yml` and `openrouter.py` read that path; moving the file in P1 would break them weeks before their readers move in the switch commit |
| D18 | The **ZDR guard** (`yyc/tests/test_zdr.py`) is rewritten in A1 to check both stacks while both exist (10.1.1), and loses its legacy half in A6 | the P1 layout moves the jarvis3 remote base the old guard parses; both stacks call the model during the overlap |

---

## 4. Live numbers that size the plan

| What | Value (2026-11-02) | Used for |
|---|---|---|
| Conversations (CloudCLI sessions) | 28 in 10 schemas (26 transcripts) | import time, checklist size |
| Transcript bytes | 232 MB, 9,648 lines, largest line 1.1 MB, 453 images; about 100 MB of distinct blobs after dedup | bulk import ≤ 10 min; handover reads |
| Auto-memory | `projects/-workspace/memory/` present and empty for 10 people (inventory A8.F18) | nothing to carry today; per conversation from the switch on (decision 6, 7.4.2) |
| Shares | 9 live (`cc_shared.shares`) | share pass |
| view_as audit rows | 39 | magpie import |
| Failed turns | 2 rows | magpie import |
| `egress_log` | 4,783 rows, 1.4 MB | egress copy is seconds |
| `tool_calls` | 475 rows | tools copy |
| Old sandboxes / hosts | 0 live sandboxes, 260 gone rows; `vm_hosts`: 1 active (the gateway) + 200 gone | old fabric is normally empty at night |
| Concurrency | at most 10 conversations calling the model in one 15-minute window over 30 days, p95 6 | overlap load |
| Spend | max USD 2.79 in one 15-minute window; USD 27.24 in 30 days; budget USD 100 | budget error of the overlap (section 8.3) |
| Model calls by HKT hour (14 days) | 00h 17, 01h 37, 02h 3, 03h-05h 0, 06h 6, 07h 16, 08h-11h 190-433 | the switch runs whenever it is ready (D5); the A1 ui rollout waits for a moment with no running work on the old pods (DESIGN 9.6), in practice an evening or night |
| Old drains | ui 900 s (grace 960 s), web 300 s, egress 120 s, cloudflared grace 3 min | the A1 ui rollout's drain (run with no client connection, child or chat on the old pods, so its close-all has nothing to cut); the old connectors go to 0 only after the move left them no socket; A5 scales down only what nothing uses |
| App timing | reconnect backoff 1.5 s doubling to 30 s; RPC timeout 60 s; heartbeat every 15 s with `appVisible` | a moved tab is back within about 2 s (DESIGN 9.3) |

Targets (checked by the runner, DESIGN 9.9): fence to release p99 ≤ 300 ms, max 3 s; every zero counter 0 from
T0 − 5 min to the end of A5; the move done within minutes of the settle time; A4 as long as the longest running old
turn plus its old sandbox's 30 min idle release.

---

## 5. Two stacks side by side

### 5.1 Topology during the overlap

```
                                   Cloudflare edge (Access app staff_ai, same aud)
                       DNS ai.yyc-internal.com ──► tunnel yyc-ai  (before T0)
                                               └─► tunnel pigeon  (from T0)
   ┌──────────────── ns jarvis (legacy, frozen) ───────────────┐   ┌──────────────── ns yyc-ai (new) ─────────────────┐
   │ cloudflared ──► ui (router + CloudCLI children)           │   │ cloudflared ──► pigeon ──► magpie ──► hermit      │
   │                 │  └─/proc──► old sandboxes (10.200/16)   │   │                              ▲         │         │
   │                 └► web (scaler, janitor) ─► yyc-sbx-gw    │   │                              │         ▼         │
   │ egress (old) ◄── old sandboxes  tools (old)               │   │ egress (new) ◄── hermit-gateway ◄── supervisor in │
   │ DB: jarvis, cloudcli                                      │   │ tools (new)      new sandboxes (10.203/16)        │
   └───────────────────────────────────────────────────────────┘   │ DB: magpie, hermit, egress, tools                 │
              │ (read only; exec into ui/web pods for holds)       │ magpie import ── reads cloudcli, jarvis, /jfs ──┐ │
              └────────────────────────────────────────────────────┤ (sole writer of migrating conversations)       │ │
                     JuiceFS yyc-ai: ws/<id> (both), users/, sent/ (old; read by the importer)                        │ │
                                                                   └──────────────────────────────────────────────────┘
```

The left box is the old k3s cluster, the right box the new Talos cluster (decision 13, DESIGN 15); the databases
`hermit`, `egress`, `tools`, `jarvis` and `cloudcli` are all on Cluster `pg` in the old cluster (the right box reaches
them through `db/pg-shared`), `magpie` on `magpie-pg` in the new one. For the window (A1-A6) more links cross
(DESIGN 9.1, 15.5): pigeon → ui 8080 (the legacy upstream: legacy conversations' timelines, live streams and writes)
and `magpie-import` → ui 8080 (LU reads) through bridge B1 (`yyc-bridge-old`, admitted by `jarvis/ui-from-bridge`);
the router's pass-on → pigeon through B2; `magpie-import` → the old Kubernetes API (exec for the running sets, the
pod proxy for the switch module) through B7. Established sockets on tunnel `yyc-ai` stay there after T0 only until the move,
minutes later; then the old connectors go to 0.

### 5.2 What runs when

| Thing | A1 dark | A2 import | A3-A4 window | A5 → P4 (no wait) | A6 delete | P6 (+30 d) / A7 (+90 d) |
|---|---|---|---|---|---|---|
| old cloudflared (tunnel `yyc-ai`) | 2 | 2 | 2 until the move, then 0 (A3) | 0 | gone | |
| ui (router with the switch module, from one ordinary rollout in A1) | 2 | 2 | 2 (children serve legacy conversations through LU) | 0 at A5 (back up only on a P4 rollback) | gone | |
| web, old egress, old tools | 2 each | 2 each | 2 each | 0 at A5 | gone | |
| old fabric (gateway, hosts) | live | live | live, hosts only for old turns | gateway idle, 0 hosts | torn down | |
| new cloudflared (tunnel `pigeon`) | 2 | 2 | 2 | 2 | 2 | 2 |
| pigeon, magpie, hermit, new egress, new tools | 2 each, test host only | same | serving staff | serving | serving | serving |
| hermit fabric | live, spare 0 | live | live, warm hints from the importer | live | live | live |
| `magpie import` | — | Jobs: `plan`, `run` + `verify`, `catch-up` as needed; upload ingestion | Deployment `magpie-import` (2 replicas, `handover`, running mirror, writer sidecar) | the same, idle | `import-done` deletes it after P4 | |
| `hermit-import`, `egress-import` CronJobs | suspended | every minute | every minute | every minute | final run, then gone | |
| `tools-import` CronJob | suspended | one run | — | — | final run, then gone | |
| window code (LU, tickets, fence, `migrating`, the router's switch module) | shipped | rehearsals | in use | idle (rollback) | dropped | |
| legacy-path set (`import_uploads`, `uploads.legacy_path`) | shipped | filled | in use | in use | kept | dropped at A7 |
| databases `jarvis`, `cloudcli` | live | live | live | live (rollback) | archive + final dump, dropped | dumps expire; the archive stays |
| JuiceFS `users/`, `sent/` | live | live | live for legacy conversations | frozen (rollback source) | frozen | removed (P6) |

---

## 6. Who owns a conversation

Text of record: DESIGN 9.2 and 9.4 (with switch-final 8 and 23 C5 for the fence). This section keeps what the
importer implements.

### 6.1 States

```
                 (A2 bulk import)                     (hand-over rule, 6.3)
  [legacy-only] ──────────────────► [legacy] ──fence──► [fenced] ──release txn──► [owned]
     no magpie row                    magpie row,        writer: nobody              migrating = false
                                      migrating = true   (≤ 3 s; native writes       writer: magpie only (forever)
                                      writer: the old    wait in magpie)
                                      stack (old path       │
                                      until the move,       └── unfence (a fingerprint changed) ──► [legacy]
                                      then LU + ticket);
                                      magpie copy: the importer only
  [owned] ── reverse fence + export (14.2) ──► [legacy]        (rollback only)
```

A conversation created on the new stack is born `owned` (from T0, every create is native). A tab reaches a
`[legacy]` conversation's timeline and live stream through LU from the person's old child, and its writes through LU
under a ticket. There are no per-person states (DESIGN 9.2).

### 6.2 Invariants (tested, 7.12 and DESIGN 9.9)

1. A conversation with `migrating = true` is changed in magpie only by the importer's transactions, each of which
   locks the row and re-checks `migrating = true` before writing.
2. A conversation with `migrating = false` is never written by the importer (same check; the importer's write
   functions take the row lock and abort the pass for that conversation on `false`).
3. magpie grants no lease for a migrating conversation, so no new-stack sandbox can write it; a native mutation of a
   fenced conversation waits for the release (≤ 3 s), and of an unfenced legacy one answers 409 with `route:
   "legacy"`, which pigeon turns into an LU write (only when its owner cache is stale; never reaches a tab).
4. The release transaction is the importer's last write to that conversation, and it commits only after a
   byte-exact verification (7.7) and a clean re-stat of every fingerprint inside the fence.
5. Before release, the old system can write a conversation; after release it cannot: there is no old path (the gate
   requires every ui pod to report zero non-LU sockets and the old connectors at 0), no ticket, nothing running and
   no old sandbox, and pigeon sends LU writes only for conversations magpie says are legacy.

### 6.3 The hand-over rule

The rule is DESIGN 9.4: gate set (DESIGN 9.2: the flip settled, every ui pod, enumerated from the Kubernetes API
with Terminating pods included, reporting zero first-hop non-LU sockets for 60 s after the last move, the old
connectors at 0); the conversation in no running list in two consecutive reads that started after its last ticket
closed on an outcome; 60 s quiet; no old sandbox row not `gone` (the importer waits for the old janitor's own idle
release and never releases an old sandbox); pre-verified with every fingerprint unchanged.

**Running sets, per session id** (unchanged). Each running-set read lists every `app=ui` pod in `jarvis` from the
API, Terminating included, and runs one fixed script in it (`POST …/pods/<pod>/exec`, container `ui`, `node -e
<holds.js>`, 8 s timeout; the script is a constant in the importer, `crates/magpie-import/src/holds.js`): for each
process in `/proc` whose environment has `DATABASE_SCHEMA`, `SERVER_PORT` and `JARVIS_CC_OWNER` (the router's child
environment, `jarvis/router.mjs:480-495`), it calls `GET http://127.0.0.1:<SERVER_PORT>/api/providers/sessions/running`
with `x-jarvis-user: <JARVIS_CC_OWNER>` (the call the router itself makes, `runningChats()`), and prints one JSON line
`{"schema", "ok", "sessions": [<sessionId>…]}`. That route lists CloudCLI's own runs, background-task holds and busy
Claude CLI sessions by app session id (`sessions.service.ts:207-240`), and the app session id is the conversation id. A
child that does not answer with 200 within 5 s prints `ok: false`, which makes every conversation of that schema "not
yet" (fail-closed). The quiet-period check reads the old child's `pendingUserMessages` and preparing state for the
conversation through LU. The script also reads the busy route's `asking` list (claudecodeui fd1b3ed5, DECISIONS 17:
the parked questions, `{sessionId, requestId, toolName, toolUseId, askedAt, answered}`): an answered question, whose
turn has yet to start, puts its session in `sessions` (running); an unanswered one holds nothing; an `asking` of another
shape is `ok: false` and an image without it has no parked questions. The same reads feed the **running mirror**: the importer writes each legacy
conversation's running state into magpie's `run_state` every 5 s, so native lists show old turns as running.

The runner sets the gate (`magpie import allow-handover`) after `move` and `old-connectors-0` (11.1); the per-person
form (`--person bot@deyaochen.com`, which requires only that person's zero non-LU sockets) is for rehearsal R1.

### 6.4 magpie's behaviour for a migrating conversation

| Call | Answer |
|---|---|
| reads (M1, M2, M9, M10, search, M12, M14, files list, M15, M16 reads) | normal: served from the verified copy (pigeon uses them for lists and share views; a tab's timeline of a legacy conversation comes through LU from the old child) |
| a native mutation (M5, M6, M7, M8, M11, M16 create, share revoke) of a **fenced** conversation | waits for the release (≤ 3 s), then proceeds natively; if the fence is lifted instead, answers 409 `{"code": "conversation_migrating", "route": "legacy"}` |
| a native mutation of an **unfenced legacy** conversation | 409 `{"code": "conversation_migrating", "route": "legacy"}`: pigeon re-routes it through LU at once (its owner cache was stale); never shown to a person (`magpie_conversation_migrating_total` reaching a tab must stay 0) |
| `POST /v1/conversations/{id}/legacy/tickets`, `…/tickets/{t}/done {outcome}` | switch-final 4.3 and A §3.2 (tickets only; no anchors, presence or person state); a ticket closes only on the turn's outcome (C5.1) |
| M16 `POST /v1/shares/{shareId}/continue` of a share of a migrating conversation | routed by where the share lives; the new conversation is born owned |
| supervisor `POST /v1/sandbox/lease` for it | 409 `conversation_migrating` (no new-stack turn is admitted for it) |
| magpie's own jobs (purge, title commands, watchdog, driver sweeper, blob GC of its refs) | skip migrating conversations |
| hermit sandbox events for it | ignored (there is no new-stack sandbox for it) |

### 6.5 pigeon's behaviour

pigeon's legacy upstream (`crates/pigeon/src/legacy/`, DESIGN 9.3-9.5, with switch-final 4.1's LU connection):
native sockets only, with per-conversation routing (legacy → LU from the person's old child, owned → magpie); every
legacy write under a ticket; one `agent.timeline.replacement` per subscribed socket at a conversation's release;
uploads into magpie under the old route's limits and copied into the old child at send time for a legacy
conversation; the legacy view-as key; the request-free-first drain. No proxy sockets, seq remap, presence or move
rule. pigeon has no "being moved" sentence, no 45 s retry and no `legacy.cut` sentence: none of them can occur.

### 6.6 An old turn still running

It finishes on the old stack (DESIGN 9.5): before the move old-path tabs see it directly, pigeon tabs through LU in
real time, and after the move every tab through LU (the move closes client sockets only, never the child or its
`/proc` connection); Stop goes to the old child through a ticket; holds and CLI follow-ups keep the conversation in the running
list, so it is not released until they end. The bounds are the old system's own (30 min hold ceiling, 8 h sandbox
lifetime). No import-origin turn is synthesised and no turn is cut. An ask in an old turn waits until Stop, as it
always has on the old stack (DESIGN 9.5).

`NS` for every uuidv5 in this spec = `6f1c9d2e-0b7a-4f53-9a43-5b2f3f1e8c61` (hermit 3.8's and magpie 12's `SHARE_NS`).

---

## 7. The importer (`magpie import`)

### 7.1 Where it lives, what it is

- Library crate `aviary/crates/magpie-import`, linked into the `magpie` binary as the subcommand family `magpie
  import …` (platform R15, magpie 19). Every row, blob, event, timeline row, change, NOTIFY and cc write goes through
  the same functions the magpie server uses (cc writes through magpie 6.6/6.10's transaction, minus the lease check,
  plus the `migrating` check), so an imported conversation is exactly what live writes would have made.
- Image: the pinned `ghcr.io/de0ch/magpie:<sha12>` (the same pin as the magpie Deployment; no separate package). CI:
  aviary `ci.yml` (platform R17) runs the tests of 7.12 with the rest of magpie's.
- Removed from aviary in P5 (the crate and the subcommand, magpie's contract release, 10.5 step 8).
- Where it runs:

| What | Kubernetes object (ns `yyc-ai`) | When |
|---|---|---|
| `magpie import plan` | Job `magpie-import-plan` | A2 step 2; again in preflight; continuously inside the hand-over loop (C15) |
| `magpie import run` then `verify --all` | Job `magpie-import` (platform 9.12) | A2 step 3, once |
| `magpie import catch-up` then `verify --all` | Job `magpie-import-catch-up-<n>` (a new name per run) | in A2 after a long gap or a fix, optional; it is the same sync code as the final sync |
| `magpie import handover` | Deployment `magpie-import`, 2 replicas (`RollingUpdate maxSurge 1 maxUnavailable 0`, PDB `minAvailable 1`, hostname spread, readiness `:9090/readyz`, preStop 5 s, grace 60 s), leader by `pg_try_advisory_lock(hashtext('magpie-import.leader'))` on the magpie database, held for the process's life; the pod without the lock is ready and waits. The leader also runs the running mirror, upload ingestion, warm hints and the gate's socket checks (DESIGN 9.1-9.4); container `export-server` is the writer sidecar of 14.3 | from `cutover start-handover` (A3) until P4 ends, then deleted (`cutover import-done`) |
| `magpie import allow-handover [--person <email>]` | the same Deployment | A2 rehearsal R1 (a per-person gate for bot); A3 for everyone |
| `magpie import export --since <t>` | the writer sidecar | a reverse hand-over only (14.2-14.3) |
| one-shot commands (`status`, `allow-handover`, `hold`, `unfence`, `reverse`, `release --force`, `holds`) | `kubectl -n yyc-ai exec deploy/magpie-import -- magpie import <cmd>` while the Deployment exists, else a Job | they take the same lock; the loop releases and retakes it between passes when a `pg_advisory_lock` waiter is present (`pg_locks`) |

The Deployment's container is named `magpie-import` with args `[import, handover]`, so platform 13.3's no-switch rule
for containers named `magpie` (args exactly `[serve]`) is untouched; it meets 13.3's zero-downtime shape as written.

### 7.2 Inputs, credentials, placement

| Input | How | Credential |
|---|---|---|
| database `cloudcli` | session option `default_transaction_read_only=on` for every command except `export`, and on every connection but one small writable pool that only takes a releasing conversation's old schedules (`cc_shared.schedules`, `schedule_runs`: locked `for update`, marked `moved`, in a transaction held open around magpie's release transaction, committed after it; aviary magpie-import `schedules::claim`); each pass in one `repeatable read` transaction | Secret `magpie-import` (platform 12.2): `MAGPIE_IMPORT_CLOUDCLI_URL` = `YYC_CLOUDCLI_DATABASE_URL` |
| database `jarvis` | same, always read-only | Secret `magpie-import`: `MAGPIE_IMPORT_JARVIS_URL` = `YYC_JARVIS_DATABASE_URL` |
| JuiceFS `/jfs` | PVC `hermit-jfs` (platform 9.6, hermit 15.1) mounted read-only at `/jfs` (`readOnly: true` on the volume and the mount) through the new cluster's JuiceFS CSI (metadata through `db/pg-shared`); required node affinity to nodes in network `yyc` (label `yyc.hetzner/network=yyc`, fsn1 preferred; DESIGN 15.7). Only the export Job mounts it read-write | — |
| magpie database and bucket | the magpie library | Secret `magpie` (platform 12.2): `MAGPIE_DATABASE_URL`, `MAGPIE_S3_*` |
| running sets (6.3) | `pods/exec` into `app=ui` (container `ui`) pods in ns `jarvis` through the **old** cluster's API server (`https://10.0.0.3:6443`, bridge B7); ui pods enumerated from that API, every phase | ServiceAccount `jarvis/magpie-import` on the old cluster with Role `magpie-import-legacy` (`pods` get/list/watch, `pods/exec` create, `pods/proxy` get/create) and RoleBinding, and its token Secret `jarvis/magpie-import-token`, all in `yyc-ai/legacy/` (so they go with the namespace); the token reaches the importer as `MAGPIE_IMPORT_LEGACY_KUBE_TOKEN` in Secret `yyc-ai/magpie-import` (store `YYC_OLD_CLUSTER_IMPORT_TOKEN`), the old CA from `infra/yyc-cluster-ca.pem` |
| the router's switch module (gate check) | `GET /api/v1/namespaces/jarvis/pods/<ui pod>:8080/proxy/__jarvis/sockets` on every ui pod through the old API (B7), the module's token in `x-jarvis-switch-token` (the API server's proxy does not pass `Authorization` on); the runner calls `POST …/__jarvis/move` with the same token from the session (`cluster.sh ssh`) | `MAGPIE_IMPORT_TOKEN_UI` = `YYC_TOKEN_MAGPIE_IMPORT_UI` in Secret `magpie-import` |
| quiet period (pending messages, preparing) | through pigeon's LU (pigeon holds the person's newest token): magpie asks pigeon for the old child's pending/preparing state of the conversation before a fence | — |

The secret naming follows platform 9.12/12.2 (`<service>-import` Secrets holding only the legacy source URLs, the
service's own Secret for its own bindings); no peer token of the old router is needed, because the running sets are
read inside the ui pods.

NetworkPolicy `yyc-ai/magpie-import-egress` (new cluster): `magpie-pg` pods 5432, `10.0.0.2/32:30432` (`pg` through
`pg-shared`, B3), the bucket endpoint 443, the old API `10.0.0.3/32:6443` (B7), `10.0.255.1/32:8080` (LU reads
through `yyc-bridge-old`, B1), hermit 8080 (warm hints), DNS. On the old cluster: `jarvis/ui-from-bridge` admits
`10.0.255.1/32` to ui 8080 (DESIGN 15.5), with the ServiceAccount, Role and RoleBinding above.

Resources: request 200m / 512Mi, limit 1 CPU / 512Mi (memory request = limit); the largest single read is one
transcript (tens of MB) streamed line by line, with the 1.1 MB line cap of magpie 17.

### 7.3 Its own tables (in the magpie database; shipped as magpie migrations, dropped in P5)

```sql
-- One row per imported conversation. Native conversations have none.
create table import_sources (
  conversation_id   uuid primary key references conversations on delete cascade,
  person            text not null,
  user_key          text not null,           -- 'u' || left(sha256hex(person), 24), checked against the schema name
  source_schema     text not null,           -- cc_u...
  provider_session  text,                    -- Claude Code session id (the transcript's file name)
  row_fp            text not null,           -- sha256 of the imported sessions-row columns (7.4.1), canonical JSON
  files             jsonb not null,          -- {"<cc path>": {"src": "<path under /jfs>", "size": int, "mtime_ns": int,
                                             --   "ino": int, "sha256": "<hex of the whole source file at last sync>"}}
  sent_fp           text not null,           -- sha256 of the sorted (path, size, mtime_ns) list under sent/<conv>/
  uploads_done      text[] not null default '{}',   -- upload file names already attached
  failed_turns_fp   text not null default '',
  first_imported_at timestamptz not null,
  last_synced_at    timestamptz not null,
  last_change_at    timestamptz not null,    -- last pass that saw a source change
  verified_at       timestamptz,             -- pre-verification (switch-final A §3.3); the release only re-stats
  verified_stats    jsonb,                   -- (size, mtime_ns, ino) of every source + every fingerprint at verify
  released_at       timestamptz,             -- set null again by a reverse hand-over (14.2)
  release_reason    text check (release_reason in ('idle', 'forced')),
  last_error        text
);
create index import_sources_open on import_sources (person) where released_at is null;

create table import_shares (
  share_id     text primary key,               -- cc_shared.shares.id (22 chars)
  src_fp       text not null,                  -- sha256 of the row + the share folder listing
  imported_at  timestamptz not null
);

create table import_control (
  id                   boolean primary key default true check (id),
  handover_allowed_at  timestamptz,            -- the gate (6.3)
  hold                 boolean not null default false,  -- forces phase sync (no release)
  connectors_off_at    timestamptz,            -- the runner recorded the old connectors at 0 (the gate's input)
  all_owned_at         timestamptz,            -- every import_sources row released
  export_epoch         bigint not null default 0        -- 14.3
);
insert into import_control default values;
```

`conversations.migrating` (magpie 4) is the flag the rest of magpie reads. The importer sets it `true` when it
creates the conversation and `false` only in the release transaction (and `true` again only in a reverse hand-over).
The window's other tables (`legacy_tickets`, `import_uploads`, columns `conversations.legacy_fence_at` and
`uploads.legacy_path`) are magpie's (DESIGN 5.2, 9.4-9.5); the legacy-path set (`import_uploads`,
`uploads.legacy_path`) lives until A7. There are no anchors, seq maps, presence or person-state tables.

### 7.4 What a pass copies

A pass = (1) one `repeatable read` read of `cloudcli` (every `cc_u*` schema: `sessions`, `jarvis_failed_turns`; and
`cc_shared.shares`) and of `jarvis` (`ops_events` kind `view_as`; `sandboxes` non-gone rows); (2) a stat walk of the
source files of every non-released conversation; (3) for each conversation whose `row_fp`, file stats, `sent_fp` or
failed-turns fingerprint changed, a sync in one magpie transaction per file operation; (4) the share and view-as
passes; (5) in `handover` only, pre-verification of idle conversations, the hand-over rule (6.3), the running mirror and
upload ingestion (DESIGN 9.3-9.5). `run` is one pass over everything with no
fingerprints yet (the bulk import); `catch-up` is one pass; `handover` repeats passes every 5 s from the end of the
previous one. All three use the same sync code, so the final sync of a conversation at its handover is exactly a
catch-up of that one conversation.

#### 7.4.1 Sessions rows (magpie 19.1 mapping, unchanged, plus)

- A new sessions row (any schema, `session_id` uuid, or disk-discovered with `session_id = provider_session_id`)
  creates the conversation with the same id, `migrating = true`, `origin = {"kind": "import"}`, `conversation.created`
  event with `origin.kind = "import"`, and its `import_sources` row.
- `row_fp` covers `custom_name`, `name_source`, `"isArchived"`, `deleted_at`, `provider_session_id`, `jsonl_path`,
  `created_at`, `updated_at`, `model`. A change maps to the same magpie changes a person's action would make
  (title with `name_source`, soft delete with `deleted_at`, restore), each with its list change and events, but
  written by the importer.
- A sessions row that disappeared (CloudCLI's 30-day purge, a permanent delete, the synchronizer's prune, A17.F3,
  A17.F6, A17.F8) while `migrating`: magpie's purge of that conversation (rows, refs; blob GC as usual), and its
  `import_sources` row with it. After release a vanished source changes nothing.
- A conversation with no transcript file yet (created, first message never answered): imported with its failed turn
  only; a transcript that appears later is synced normally.

#### 7.4.2 cc files

Sources per conversation, with their cc path (the path under `~/.claude` that the supervisor's FUSE view shows, so
`claude --resume` reads the same names):

| Source under `/jfs/users/<key>/claude/` | cc path |
|---|---|
| `projects/-workspace/<psid>.jsonl` | the same (`main_transcript`) |
| `projects/-workspace/<psid>/tool-results/**` | the same |
| `projects/-workspace/<psid>/subagents/**` | the same |
| superseded transcripts (`superseded_provider_sessions`, 0 rows live) | not imported |
| `projects/-workspace/memory/**` (Claude Code's auto-memory, shared today by all of a person's conversations because the cwd is always `/workspace`) | **per conversation** (decision 6): present and empty for 10 people today (A8.F18). A file that appears is copied into the cc scope of **each** of that person's imported conversations, at import and again at that conversation's release (so each copy holds what the person had when it moved, then diverges); `plan` reports the files; nothing blocks the importer and nothing is dropped. The importer ignores `aviary-memory/` (14.3) |

Sync of one file (`S` = source bytes, `M` = magpie's stored copy, both known by size and per-line sha256):
1. Stat unchanged (size, mtime_ns, ino) → nothing.
2. New file → create (lines or bytes kind by magpie 6.2) and write `S` at offset 0.
3. `size(S) ≥ size(M)` and the bytes of `S` at `[size(M) - k, size(M))` equal M's last `k` bytes, where `k` =
   min(size(M), length of M's last complete line plus its tail) → append write at offset `size(M)` of
   `S[size(M)..]` (magpie 6.6 fast path, or the torn-tail path when M has a tail).
4. Otherwise (a rewrite: compaction, tombstone removal, relocation) → read `S` line by line, find the first line
   whose sha256 differs from M's line at the same ordinal; write `S[that line's offset..]` at that offset, then
   truncate to `size(S)`. magpie's matching (6.6 step 5) keeps the events of unchanged and moved lines and retracts
   removed ones (6.7), exactly as for a live sandbox.
5. Source file gone → unlink.
6. Directory renames (Claude Code relocation) appear as a gone file plus a new file; the importer detects the pair by
   identical sha256 within one pass and issues a rename instead (keeps events).

Writes are chunked at 16 MiB (magpie's write cap). Each write is its own magpie transaction with the importer's
`op_id = uuidv5(NS, conversation_id + ":" + path + ":" + offset + ":" + sha256(chunk))`, so a pass cut by a crash
repeats harmlessly. Only complete lines produce events (magpie 6.3); a half-written tail is stored as tail and
completed by the next append.

Events: the main transcript is classified with the live classifier (magpie 6.5, `main_transcript` set before the
first write). Import display fixes (magpie 19.3) apply to every line the importer writes, not only to the first
import.

#### 7.4.3 Uploads, sent files, failed turns

- Upload ingestion (switch-final 3.1 F3, DESIGN 9.5): from A2 every file under each person's
  `users/<key>/claude/cloudcli-assets/` goes into `import_uploads` (bytes to the bucket), the whole folder on the first
  pass, new files every 5 s, and one more pass of the person's folder inside each of their conversations' release
  transactions, so a file attached in an old tab before the move and sent natively after release resolves.
- Uploads: on a newly written prompt line with an `<image_uploads>` or `<files_input>` block, each named file under
  `/jfs/ws/<conv>/uploads/<name>` (or `~/.claude/cloudcli-assets/...` for images) is stored as a blob and attached
  as `meta.attachments` of that prompt's event (magpie 19.1). A file missing on disk is recorded as an attachment
  with `"missing": true` (pigeon shows its name without a link).
- Sent files: each new `sent/<conv>/<toolUseId>/` directory becomes blobs, `sent_files` rows and one `files.sent`
  event. On the first import it is placed after the SendUserFile `tool_use` (magpie 19.1); in later passes it is
  appended at the next seq, which is after the `tool_use` because the old hook snapshots before the tool result is
  written.
- Failed turns: rows of `jarvis_failed_turns` not yet imported (fingerprint = sorted ids) become `user.sent` (with
  text) and `turn.ended` failed (magpie 19.1 rules); in later passes they are appended at the next seq.
- Parked questions (`jarvis_open_questions`, schema 4; DECISIONS 17) are not imported by the passes: the release
  reads them inside its fence and, in the release transaction, carries every unanswered one over (magpie-import
  `questions.rs`): a parked turn (origin person, uuidv5 of the first request id, asked at the question's time) that is
  the conversation's active turn, and one `ask.pending` per question (`askId` = the old request id, `toolUseId`, the
  input), so the question stays on the person's screen and their answer resumes the conversation in a new sandbox
  (magpie 8.1a, supervisor 7.6). An answered question unfences an idle release (its turn has yet to run on the old
  stack); a forced release carries it with its answer, which admits the turn at once.

#### 7.4.4 Shares

For each `cc_shared.shares` row whose `src_fp` changed: create or update the magpie share with the same id
(magpie 19.1 row "cc_shared.shares live rows"); a revoked one becomes revoked, its frozen data removed. The frozen
workspace is `ws/<uuidv5(NS, "share:" + id)>`, made by hermit's share pass (8.1); a share imported before its clone
exists is stored and works except for workspace file downloads, which answer 404 until the clone appears (at most
60 s). A share whose conversation is migrating is still viewable and continuable; requests are routed by where the
share lives; a revoke through LU is reflected in magpie on the ticket's NOTIFY.

#### 7.4.5 View-as audit

`jarvis.ops_events` kind `view_as` rows with an id above the last imported one → magpie `view_as_log`. New rows can
appear until ui is gone (A5).

### 7.5 The hand-over loop

```
loop (magpie import handover):
  take or keep the leader lock (7.1)
  pass_started = now()
  read cloudcli + jarvis (one snapshot each); list ui pods from the API (every phase)
  read every ui pod's /__jarvis/sockets (fresh = generated in this pass); record non-LU and LU socket counts
  for each conversation c with released_at is null, in order of last_change_at desc (and at once on a ticket NOTIFY):
      if fingerprints unchanged: (re-)pre-verify if not yet verified; continue
      sync(c)                              # 7.4.1-7.4.4; errors recorded in last_error, pass continues
  share pass; view-as pass; upload ingestion; plan (cheap re-run)
  sets = last two running-set reads        # 6.3: their own task, exec per ui pod, 5 s cadence, 8 s timeout
  running mirror: write run_state of every legacy conversation from sets
  if gate set (and, for a per-person gate, only that person's conversations):
      for each open c:
          if releasable(c, sets):          # DESIGN 9.4 conditions 1-5; every ui report fresh, 0 non-LU sockets (opaque included), 0 in-flight non-LU HTTP
              quiet check through LU -> fence(c) -> re-stat every fingerprint -> release(c, 'idle')   # or unfence
  warm hints to hermit (every 60 s while any conversation is legacy)
  publish status (7.9)
  sleep until pass_started + 5 s (or at once when the pass took longer)
```

A pass never holds the leader lock's transaction open; every write is its own short transaction. The fence has a
3 s lease: if the leader dies between fence and release, the other replica takes the lock, re-stats and either
releases or unfences. The first pass after the Deployment starts is a catch-up of everything changed since the A2
bulk import. At the gate the first pass typically releases every idle conversation (28 today, each ≤ 300 ms).

### 7.6 Size and time

| Operation | Bound |
|---|---|
| First `run` (28 conversations, 232 MB, ~100 MB blobs to the bucket at fsn1) | ≤ 10 min, 8 conversations in parallel |
| `verify` of everything | ≤ 5 min (reads every source byte once, reconstructs every file once) |
| Catch-up of everything since P2 (first handover pass) | ≤ 2 min for a week of normal use |
| Hand-over pass with nothing changed | ≤ 1 s (28 rows, ~60 stats); the running-set execs run beside it (≤ 8 s each, 5 s cadence) |
| Hand-over pass with one active old turn | ≤ 2 s (append of the new bytes only) |
| Fence to release of one pre-verified conversation | p99 ≤ 300 ms, max 3 s (one re-stat of ≈ 60 files and the fingerprints, two short transactions) |
| Rewrite of a 30 MB transcript | ≤ 20 s |
| Pre-verification of one conversation | final sync + verify of its files: ≤ 5 s for a 30 MB transcript (done while idle, before any fence) |
| Memory | one line (≤ 1.1 MB) plus one 16 MiB write chunk plus 8 parallel blob uploads |

### 7.7 Verification

For each cc file of the conversation: reconstruct through magpie's own read path (magpie 6.11, the code the FUSE
layer reads through) and compare sha256 with the source file read in the same final sync. All files equal → set
`verified_at`. Any difference:
1. log `reconstruct mismatch` (conversation, path, sizes, first differing offset), metric `import_verify_failed`;
2. re-ingest that file from offset 0 (replace-all write, magpie 6.6 with `C = 0`), verify again;
3. still different → the conversation stays legacy with `last_error`, status `stuck`, and is shown on hermit's ops
   page (problem: "conversation <id> could not be verified"). It keeps working fully through the old path and LU
   (nobody sees anything); it blocks only A5. The operator fixes the importer and redeploys, or, after inspection,
   runs `magpie import release <id> --force` (writes `release_reason = forced` and the mismatch in the log).

Also checked once per conversation at first import and again at release: event count and `user.sent`/prompt count
against the classifier's count of the source (sanity, not a gate), and that the conversation's workspace `ws/<id>`
exists on JuiceFS (a missing workspace is created at first start by hermit; logged only).

### 7.8 The fence and the release transaction

```sql
-- fence (no open ticket, quiet period checked)
begin;
select migrating, legacy_fence_at from conversations where id = $1 for update;          -- must be true, null
select 1 from legacy_tickets where conversation_id = $1 and done_at is null limit 1;     -- must be none
update conversations set legacy_fence_at = now() where id = $1;                           -- M12 conversation_upserted
commit;
-- re-stat every source file and fingerprint against import_sources.verified_stats (outside any transaction)
-- release
begin;
select migrating, legacy_fence_at from conversations where id = $1 for update;          -- must be true, set
-- one more upload-ingestion pass of the person's cloudcli-assets/ (rows written in this transaction)
update conversations set migrating = false, legacy_fence_at = null, run_state = 'idle',
       model = $old_child_model, name_source = $mapped_name_source where id = $1;
update import_sources set released_at = now(), release_reason = $reason where conversation_id = $1;
-- M12 conversation_upserted (migrating false) + NOTIFY; pigeon sends one timeline replacement per subscribed socket
commit;
```

A changed stat between the two lifts the fence instead (`unfence`: `legacy_fence_at = null`, NOTIFY); waiting
ticket calls then answer `legacy` and go through LU. After the release commit, writes waiting in the fence go native
at once (the first native turn of the conversation).

### 7.9 Command line and status

| Command (`magpie import …`) | Does | Writes |
|---|---|---|
| `plan` | counts, sizes, schemas, id conflicts with native magpie conversations (must be 0), shares, failed turns, uploads found/missing, the largest lines, auto-memory files (must be 0, 7.4.2); prints JSON; exit 1 on a blocker | nothing |
| `run` | the bulk import: one full pass in phase `sync` over every conversation | rows, blobs |
| `catch-up` | one pass in phase `sync` (only what changed since the last pass) | rows, blobs |
| `verify [--all\|<id>…]` | open conversations: 7.7. Released ones: checks that every source file still has the size and sha256 recorded in `import_sources.files` at release (proves the old side wrote nothing after the handover); magpie's copy has since grown with new turns, so it is not compared | `verified_at` of open ones |
| `handover` | the loop (7.5); the Deployment's command | as the loop |
| `unfence <id>` | lift a fence by hand (normally automatic) | the row |
| `reverse <id>…` | the reverse hand-over of 14.2 | as 14.2 |
| `holds` | one running-set read (6.3), printed; P2 checks it against the old side | nothing |
| `allow-handover [--person <email>]` | sets the gate (for one person, or for everyone); refuses while any ui report is stale or shows a non-LU socket (of that person) | `import_control` |
| `hold on\|off` | forces `sync` (no release) | `import_control` |
| `release <id> --force` | 7.7 step 3; A2 rehearsal 7 | release |
| `reset --yes` | A2 only, before the gate: deletes every conversation that has an `import_sources` row and `migrating = true` (rows, refs; blobs to GC) | deletes |
| `parity <id>…` | 10.2 item 6 (prints normalised Paseo rows of magpie's copy, magpie's rows mapped by pigeon's Row → PaseoRow function; compared with the old side's output; must be equal) | nothing |
| `export [--since <iso>] [<id>…]` | the reverse export (14.3), by the writer sidecar | JuiceFS `users/`, `sent/`, database `cloudcli` |
| `status` | prints the status document | nothing |

Status document (also `GET :9090/status`, beside `/metrics`, `/readyz`, `/healthz` on the platform's ops port 9090,
which is in no Service and reached with `kubectl port-forward` or `exec`):

```json
{"leader": true, "phase": "sync|handover|done", "gate": "2026-11-05T02:01:12Z", "hold": false,
 "pass": {"at": "…", "ms": 412, "changed": 1},
 "conversations": {"imported": 28, "legacy": 3, "fenced": 0, "released": 25, "running": 1, "stuck": 0},
 "open": [{"conversation": "…", "personKey": "u1f…", "waitingFor": "gate|running|quiet|oldSandbox|verify|holdsUnanswered", "since": "…"}],
 "sockets": {"oldPath": 0, "lu": 2, "oldConnectors": 0},
 "fenceMs": {"p50": 90, "p99": 240, "max": 410}, "uiPods": 2, "reportsFresh": true, "oldLiveSandboxes": 1,
 "errors": [{"conversation": "…", "error": "…", "at": "…"}]}
```

 Logs never carry
emails or message text (user keys and conversation ids only).

### 7.10 Failure modes (importer)

| Failure | Effect | Recovery |
|---|---|---|
| importer pod dies mid-pass | writes are per-operation transactions with deterministic `op_id`s | the next leader repeats the pass |
| magpie database unavailable | pass fails, logged; fence and release wait | retried every 5 s; legacy conversations keep working through LU |
| `cloudcli` or `jarvis` unavailable | no new source data; no release | retried |
| JuiceFS mount stalls | the stat walk times out (30 s) | pass aborted, retried; no release without two good stats |
| an exec into a ui pod fails or times out, or a child answers `ok: false` | that schema's conversations stay legacy (fail-closed) | retried every 5 s; nothing visible |
| a ui pod's `/__jarvis/sockets` report is missing or stale | the gate is not set, or no new fence while it stays stale | retried; nothing visible (legacy conversations work through LU) |
| a file appears under `projects/-workspace/memory/` | copied per conversation (7.4.2) | nothing to do |
| a source line over the cap (> 1.1 MB) | that file fails with `line_too_long` | caps are magpie 17's; the census max is 1.1 MB; that conversation stays legacy and blocks only A5 |
| verify mismatch | 7.7 | stuck conversations stay legacy and block A5, never anything a person sees |
| the old system writes after release (prevented structurally, 6.2 item 5) | `verify --all` (released copies against the sources recorded at release) | the runner holds new fences and tells Deyao |

### 7.11 Rows the importer does not copy

As magpie 19.1: drafts (0), scheduled messages (0), notification and
push tables, `scan_state`, `projects`, `api_keys`, `app_config`, `superseded_provider_sessions` (0). The old chat
UI's legacy tables in `jarvis` (37 conversations of 2026-10-20/21) and the bucket prefix `yyc-ai-files/jarvis/` are
not imported: archived only, then deleted (decision 5, 15.4). (`cloudcli-assets/*` files are ingested into
`import_uploads` for legacy-path resolution, 7.4.3, not imported as conversation content.)

### 7.12 Tests (aviary `ci.yml`, with magpie's suite)

1. Fixtures generated in the test (no real data in git): ten schemas with both timestamp formats, the 1970
   placeholder, disk-discovered rows, deleted rows, failed turns with and without text, transcripts with images
   (base64 PNG/JPEG/PDF), torn tails, a 1.1 MB line, tool-results and subagents files, uploads (present and missing),
   sent files, shares (prefix and non-prefix frozen transcripts, revoked), view-as rows.
2. Byte-exact: import, reconstruct, sha256 equal for every file.
3. Sync convergence (property test): a reference source directory is mutated by random sequences of append,
   partial-line append, rewrite-from-line, truncate-then-rewrite, compaction (temp file + rename over), relocation
   (folder rename), file creation/removal and sessions-row edits, with passes interleaved at random points; after
   every pass with a stable source, magpie's reconstruction equals the source and the visible events equal the
   classification of the source's current lines.
4. Ownership: a pass never writes a conversation with `migrating = false` (a test flips it between read and write);
   every route of 6.4's table behaves as listed (fenced wait, 409 with `route: "legacy"`, tickets closing only on
   an outcome).
5. Handover: a fake Kubernetes API (pods appearing/terminating, an exec endpoint that runs `holds.js` against fake
   CloudCLI children listing session ids, children that time out or answer 500), a fake switch module
   (`/__jarvis/sockets`) and a fake `jarvis.sandboxes`: release only under every condition of DESIGN 9.4, per
   conversation; a conversation listed running is never released while its idle siblings of the same person are; a
   schema with one unanswered child releases nothing; a Terminating ui pod without a fresh report, or any report with
   a non-LU socket, keeps the gate unset; nothing is ever released by a deadline. `holds.js` itself is tested against the live ui
   image's `dist-server` in a container (the route and the child environment are read from the pinned image, so a
   changed image fails CI).
6. Fence: the DESIGN 9.9 cases (fence-race fuzz of 1 000 sends around a fence, leader `kill -9` between fence and
   release, a magpie switchover during a fence); the running mirror follows a fake running set within one pass.
7. Reverse export (14.3): import, add turns, a share, a failed turn and a memory file through a fake supervisor,
   export, re-read the exported files: equal to magpie's reconstruction; the sessions rows, the share row and the
   failed-turn rows match; the memory went to `aviary-memory/<id>/`.
8. Leader lock: two processes, one passes at a time; a one-shot command waits for the loop; killing the leader
   hands over to the other replica within one pass.
9. Auto-memory: a fixture with a file under `projects/-workspace/memory/` is copied into each of that person's
   conversations at import and at release; `plan` reports it and exits 0.

---

## 8. The other one-off copies

### 8.1 hermit (`hermit import-jarvis`, hermit 22.3 step 2, amended)

Unchanged from hermit 22.3 except:
- shares' frozen workspaces: `juicefs clone /jfs/users/<key>/shares/<id>/ws /jfs/ws/<uuidv5(NS, "share:" + id)>` then
  ensure (quota), instead of a rename; already-present targets are skipped (idempotent);
- runs as CronJob `hermit-import` in `yyc-ai` (image `hermit`, args `[import-jarvis]`, Secret `hermit` plus
  `hermit-import` with `HERMIT_IMPORT_JARVIS_URL`, platform 12.2), every minute from A2 until A6
  (`concurrencyPolicy: Forbid`, `activeDeadlineSeconds: 300`), argument `--delta` after the first run (rows with ids
  above the last imported, new share folders); a last run in A6 after the old stack is gone; then the CronJob and the
  subcommand are deleted;
- Job `hermit-jfs-owner` (hermit 15.4) runs once in A2 before the first import, and again in A6 after
  `legacy-down.sh`.

### 8.2 egress and tools (`egress import-jarvis`, `tools import-jarvis`)

New subcommands in the `jarvis3-chat` image, selected by argv like the roles (platform 17.2: `args: [egress,
import-jarvis]`, `[tools, import-jarvis]`; deleted in A6), run as CronJobs `egress-import` and `tools-import` in
`yyc-ai` with a read-only connection to `jarvis` (Secrets `egress-import`, `tools-import`: `EGRESS_IMPORT_JARVIS_URL`,
`TOOLS_IMPORT_JARVIS_URL`, platform 12.2) and their service's own Secret for their own database:

| Copy | Source → target | Mapping | Schedule |
|---|---|---|---|
| egress history | `jarvis.egress_log` → `egress.egress_log` | every column; `source_id` = the old `id` (unique), `person` = `jarvis.sandboxes.owner` by `sandbox`, else the legacy `conversations.owner` by `conversation_id`, else null (A13.F5) | first run in A2, then every minute until A6 |
| egress state | the budget's `since` and limit are configuration (`egress.json` `budget`, platform 17.2); nothing else from `kv` | — | — |
| tools history | `jarvis.tool_calls` → `tools.tool_calls` | every column; `source_id` | A2 once, A6 once (delta) |

Idempotent by `source_id`. The new egress counts the copied rows in its budget like its own.

### 8.3 Budget across the window

The window is a few hours (DESIGN 9.7), so only one direction is copied: `egress-import` brings the old stack's spend
into the new egress every minute (idempotent by `source_id`), so the new egress's budget includes it within a minute.
The old egress does not see new-stack spend; in the hours until A5 it could let old turns spend past the limit by at
most what the new stack spent meanwhile. The worst observed 15-minute spend is USD 2.79 against a USD 100 limit, and
preflight requires at least USD 10 of headroom on the new egress; budget edits are frozen from A3 to A5 (DESIGN 9.6).
Nothing is written to `jarvis.egress_log`, so no migration or writer role exists on the old database.

---

## 9. The front door

### 9.1 tofu (`news-stream/deploy/cloudflare/tofu`), changes by phase

The names are platform 14.1's, and pigeon 17.5 references the same ones: tunnel resource
`cloudflare_zero_trust_tunnel_cloudflared.pigeon` (tunnel name `pigeon`), its config and token data source of the
same name, output `pigeon_tunnel_token`, store key `YYC_CF_PIGEON_TUNNEL_TOKEN` sealed into `yyc-ai/tunnel` (key
`token`), and one signed-in path list `local.staff_ai_paths` used by both tunnels and the Access app.

P1 (one commit, nothing changes for staff):

```hcl
resource "cloudflare_zero_trust_tunnel_cloudflared" "pigeon" {
  account_id = local.account
  name       = "pigeon"
  config_src = "cloudflare"
}

locals {
  # hosts both YYC AI tunnels answer; which tunnel a host reaches is its DNS record's choice (origin_ai, pigeon_test)
  staff_ai_hosts = ["ai.${local.domain}", "ai-yyc-ui.deyaochen.com", "pigeon-test.${local.domain}"]
  # unchanged in P1: today's list (the old tunnel still needs ws and __jarvis)
  staff_ai_paths = ["api", "paseo-ws", "ws", "ops", "__jarvis"]
}

resource "cloudflare_zero_trust_tunnel_cloudflared_config" "pigeon" {
  account_id = local.account
  tunnel_id  = cloudflare_zero_trust_tunnel_cloudflared.pigeon.id
  config = {
    ingress = concat(flatten([for h in local.staff_ai_hosts : [
      {
        hostname       = h
        path           = "^/(${join("|", local.staff_ai_paths)})(/.*)?$"
        service        = "http://pigeon.yyc-ai.svc.cluster.local:80"
        origin_request = { access = { required = true, team_name = local.team, aud_tag = [cloudflare_zero_trust_access_application.staff_ai.aud] } }
      },
      { hostname = h, service = "http://pigeon.yyc-ai.svc.cluster.local:80" },
    ]]), [{ service = "http_status:404" }])
  }
}

data "cloudflare_zero_trust_tunnel_cloudflared_token" "pigeon" {
  account_id = local.account
  tunnel_id  = cloudflare_zero_trust_tunnel_cloudflared.pigeon.id
}
output "pigeon_tunnel_token" { value = data.cloudflare_zero_trust_tunnel_cloudflared_token.pigeon.token, sensitive = true }

resource "cloudflare_dns_record" "pigeon_test" {
  zone_id = local.zone
  name    = "pigeon-test.${local.domain}"
  type    = "CNAME"
  content = "${cloudflare_zero_trust_tunnel_cloudflared.pigeon.id}.cfargotunnel.com"
  proxied = true
  ttl     = 1
  comment = "YYC AI switch-over test host (removed in the switch commit)"
}
```

Both tunnels carry every host from P1 on, so the flip changes only the DNS record and no ingress rule races it
(platform 14.1, which gives the new tunnel only the test host until the switch, is to follow this, 17.6). On the new
tunnel, `/ws` and `/__jarvis` are Access-gated until the switch and pigeon answers them 404 (pigeon 5.1): more
gating, never less. The old tunnel's config (`yyc_ai`) is generated over the same `staff_ai_hosts`, so it gains a
`pigeon-test` rule to `ui` too (used by the flip rehearsal, 10.2 item 8). The Access app `staff_ai` gains the
`pigeon-test` destinations through the same list (same aud, so pigeon verifies test-host tokens exactly as production
ones). The token goes into store `yyc` as `YYC_CF_PIGEON_TUNNEL_TOKEN` (`scripts/store-put.py yyc
YYC_CF_PIGEON_TUNNEL_TOKEN=file:… --explain …`) and is sealed into `yyc-ai/tunnel` (key `token`).

A3, in the switch commit (D15, `patches/10-flip.patch`; platform 14.2):

```hcl
resource "cloudflare_dns_record" "origin_ai" {
  ...
  content = "${cloudflare_zero_trust_tunnel_cloudflared.pigeon.id}.cfargotunnel.com"   # was .yyc_ai.
  comment = "YYC AI (Access app YYC AI (staff)), and origin of ai-yyc-ui.deyaochen.com: tunnel pigeon"
}
locals {
  staff_ai_hosts = ["ai.${local.domain}", "ai-yyc-ui.deyaochen.com"]       # pigeon-test removed
  # staff_ai_paths unchanged: ws and __jarvis stay on both tunnels until A6 (old pages may exist until A5)
}
# resource "cloudflare_dns_record" "pigeon_test" deleted
```

and, in the same commit, `access.tf:58` reads `yyc/deploy/catalogue.json` (hermit 21.5). A revert of the commit
restores all of it at once.

The custom hostname `ai-yyc-ui.deyaochen.com` uses `origin_ai`, so it moves with it; `ai.yyc.deyaochen.com` stays a
301 in `yyc-edge` (unchanged). The record is proxied: browsers and the iOS app resolve the same Cloudflare anycast
addresses before and after, so no client DNS cache is involved; the edge maps the name to the other tunnel within
seconds (measured in rehearsal R0, 10.2 item 8). Requests already proxied, WebSockets included, stay on the
connection they were made on, i.e. on the old tunnel's connectors, which stay up until A5 (nothing is connected
through them then). R0 must prove that established sockets survive the record change for an hour; if they do not,
9.3's ingress-rule variant is measured the same way, and if neither keeps sockets A3 does not start (switch-final 6).

A6 (cleanup commit): delete tunnel `yyc_ai`, its config and token output; `staff_ai_hosts = ["ai.${local.domain}"]`;
`staff_ai_paths = ["api", "paseo-ws", "ops"]` (`ws`, `__jarvis` dropped, pigeon 17.5); delete `ai-yyc-ui` from
`saas.tf` (custom hostname). pigeon's CI test (pigeon 17.5) then requires `staff_ai_paths` to equal pigeon's prefixes
exactly. Nothing about the catalogue changes here (it moved in A3).

### 9.2 Connectors

- New: Deployment `cloudflared` in `yyc-ai` (platform 9.8: written in YYC, same image, args and probes as today), 2
  replicas, `--grace-period 3m`, token from `yyc-ai/tunnel`, PDB `minAvailable 1`; NetworkPolicy `pigeon-ingress`
  admits it. Readiness `/ready` = registered with the edge.
- Old: Deployment `cloudflared` in `jarvis`. It keeps serving the sockets already open on it from T0 until the move
  (minutes) and goes to 0 replicas in `old-connectors-0` (A3) once every ui pod has reported zero non-LU sockets for
  60 s, so it closes nothing the move had not already closed request-free.

### 9.3 Why not a tunnel-config flip on the old tunnel (and when it is the fallback)

It is the fallback only if rehearsal R0 shows that the DNS change closes established sockets (DESIGN 9.3; a close
that is not request-aware could fail an in-flight send): then
the flip is done by pointing tunnel `yyc-ai`'s ingress rule for the host at `http://pigeon.yyc-ai.svc` (cloudflared
applies a new remote configuration to new requests only), measured the same way. Otherwise, pointing the old tunnel's
ingress at `pigeon.yyc-ai` would need the old connectors (namespace `jarvis`, whose egress
policy allows only its own namespace, DNS and the public internet) to reach pigeon, plus pigeon admitting them, and
the old connectors would carry new-stack traffic until they were replaced. The second tunnel avoids both and gives
the same one-line rollback.

---
## 10. Phases

Each phase lists its commits (all pushed straight to `main` of the repo named; every cluster change is a commit in
`YYC/yyc-ai/deploy/` applied by workflow `yyc-ai.yml`, unless said otherwise), what runs side by side, the verification checklist, what "done" means,
and its rollback. A phase starts only when the previous one is done. Every checklist result is written to the task
record (`~/artifacts/record.md` of the session doing it) with the command, its output summary and the time.

### 10.0 P0 (A0): build and prove offline, and the deploy path

Work:
- aviary: pigeon, magpie (with the `magpie-import` library and its `magpie import` subcommands), hermit,
  supervisor, sandboxd, each with its spec's CI suite.
- jarvis3: egress and tools on their own databases, hermit token lookups, fixed-name listener, `import-jarvis`
  subcommands (8.2), argv roles (platform 17.2); the ZDR literals kept (17.5).
- claudecodeui: `pigeon-app.yml` (pigeon 17.1) with the seed of D13.
- YYC: `yyc-ai/cutover/` (the runner, section 11); the P1 layout (10.1) and every patch of 11.1 prepared as reviewed
  patch files; `catalogue.json` (hermit 21.5) with a CI step in `yyc-ai.yml`'s `check` job that its `users` and `admins`
  equal `config.json`'s while both exist; the rewritten ZDR guard (10.1.1).
- store `yyc`: every new key of platform 12.3 (32 random bytes each, `scripts/store-put.py … --explain`) and
  `YYC_CF_PIGEON_TUNNEL_TOKEN` (from output `pigeon_tunnel_token` after the P1 tofu apply).
- The switch machinery (DESIGN 9.7 A0): pigeon `legacy/`, magpie's window routes and tables, the importer's
  hand-over, the router's switch module in claudecodeui `jarvis/router.mjs` with its tests (DESIGN 9.6), the persona
  rig (instrumented web build and iOS replica), the LU contract rig, the legacy-in-a-box nightly, fence-race fuzz and
  crash cases.
- Live infrastructure, before P1 (no effect on YYC AI): on the old cluster, the `legacy/` folder move (WP-39, DESIGN
  15.4: copy, identical render, Flux path switch with `prune: false`, delete the old copy; no Flux hand-over, no k3s
  restart), after T1-T2 like the rest of the operator phase (decision 13); on the new cluster, once the builder's
  steps 1-2 are done with DESIGN 15.2.1's build inputs, its footing for YYC AI (WP-59: `talos-ready.sh` green on N1-N12, `clusters/talos/{deploy-access,yyc-ai-access,juicefs-csi}.yaml` applied by that
  cluster's workflow, tofu `deploy.tf` on, `yyc-ai.yml`'s deploy job on, deploy-workflows 12.5). Cluster `pg` is not
  changed (decision 10).

Checklist:
- [ ] aviary CI green on `main` for every crate, including: pigeon's protocol contract suite against the client
      package of every supported client (13.1), magpie's FUSE fixture test with the real Claude Code 2.1.304 binary,
      the supervisor's gVisor FUSE gate (supervisor 22), hermit's conformance suite, the importer tests 7.12.
- [ ] jarvis3 CI green: egress/tools tests on their own databases; `import-jarvis` idempotency tests; the gateway
      and model conformance suites (platform 17.3).
- [ ] The relay (staff's own Claude Code, YYC README "Relay"; DESIGN 10 row "Relay"): Deyao's decision recorded
      (kept: its work package is on `main`; dropped: the text telling relay users is ready for 11.2).
- [ ] The label store (DESIGN 10, decision 15): WP-61 on `main`, the new egress's `labels` route configured with the
      `yyc-labels-ai` keys, and the managed skills include `labels`.
- [ ] Tools over REST and managed skills (decision 15): WP-62 on `main`; a new-stack sandbox's Claude Code calls
      `http://tools/v1/tools/web_search` with `curl` as the `web` skill says.
- [ ] `pigeon-app` image contains every file of `ghcr.io/de0ch/yyc-ai-ui:<live tag>`'s `/app/dist/_expo/static/**`
      with identical bytes (`crane export` both, `sha256sum` compare; the runner's `preflight` repeats it).
- [ ] The `legacy/` move renders identically: `kustomize build yyc-ai/legacy` equals `kustomize build yyc/deploy`
      of its parent, byte for byte after sorting (a CI step in `yyc-ai.yml`'s `check` job, diff must be empty); after
      the Flux path switch both Kustomizations are Ready with no ReplicaSet hash or pod age in `jarvis` changed.
- [ ] The rewritten ZDR guard (10.1.1) passes on the prepared P1 tree, and fails on three seeded mutations: a
      non-ZDR model in `egress.json` `smallModel`, a route to openrouter.ai without `model: true`, and a
      `jarvis3-chat` tag whose commit lacks `FORCED_PROVIDER`.
- [ ] Legacy-in-a-box green on the claudecodeui commit A1 will deploy (the ui image with the switch module) and the
      aviary commit it will pin; the switch module's byte-identity and request-tracking cases green; a `move` in the
      box closed every non-LU socket only while it had no request outstanding.
- [ ] The builder's steps 1-2 are done, built with DESIGN 15.2.1's inputs, and `yyc/infra/talos-ready.sh` is green
      on DESIGN 15.2's N1-N12 (its output in the task record); the three cpx32 of N12 are workers in network `yyc`.
- [ ] The deploy path to the new cluster works (deploy-workflows 12.5 done; a no-op commit gives a green run with an
      empty plan; `kubectl auth can-i --list --as=gha:yyc-ai` on the new cluster matches deploy-workflows 4.2).

Done: all boxes ticked. Rollback: nothing to roll back.

### 10.1 P1 (A1): the new stack, dark, and the one ui rollout

Precondition (decision 13): P0 done, including the new cluster's footing (WP-59) and the `legacy/` move (WP-39).
Every commit below names the cluster it changes; new-cluster commits are applied by `yyc-ai.yml` (or, for
`clusters/talos/**`, by that cluster's own workflow), old-cluster commits by Flux (DESIGN 15.3).

Commits and steps, in order:
1. **Databases.** Old cluster: `news-stream/deploy/db/postgres.yaml`: managed roles and `Database` CRs `hermit`,
   `egress`, `tools`; `pg-dump-daily.yaml` gains the three; `pg-ingress` gains 5432 from 10.0.0.0/16 (B3) and keeps its
   10.244.0.0/16 and 10.201.0.0/24 rules (check before the push: the commit's diff of `pg-ingress` only adds lines, and
   after Flux applies it the live policy still lists both old blocks); sealed role
   passwords in `news-stream/deploy/apps/sealed/` (old cert; platform 11.1). Applied by Flux `yyc-db`. New cluster:
   `news-stream/deploy/clusters/talos/db/magpie-pg.yaml` (Cluster `magpie-pg` with database `magpie`, its
   ScheduledBackup, `magpie-pg-ingress` by selector: the instances, the operator, magpie and the importers; DESIGN
   15.8, pg-ha 4), `pg-dump-magpie.yaml`, `pg-shared.yaml` (selector-less Service with NodePort 30433, target port
   30432, and the manual EndpointSlice `pg-shared-manual` to 10.0.0.2:30432, labels `kubernetes.io/service-name:
   pg-shared` and `endpointslice.kubernetes.io/managed-by: yyc-manual`; T5 replaces both, DESIGN 15.8);
   applied by the new cluster's workflow.
2. **The bridges** (DESIGN 15.5): `yyc/infra/bridges.sh up` (the bridge server yyc-bridge1, up since
   2026-11-05; re-run so its targets are current; health checks red until the NodePorts exist); old cluster (Flux): `jarvis/ui-bridge` in `yyc-ai/legacy/`, `yyc/yyc-mcp-bridge`, `yyc/web-bridge`
   and their ingress rules from 10.0.255.1/32 in `news-stream/deploy/apps/`, `yyc/hermit-bridge` (selector-less) and
   the monitor's egress; new cluster (`yyc-ai.yml`, in the same push as 3): `bridges.yaml`. `bridges.sh check` green
   for every target that runs a callee pod.
3. **YYC `yyc/deploy/`** (platform 9.1's layout, the new stack only; `yyc-ai/legacy/` untouched):
   ```
   yyc/deploy/kustomization.yaml   resources: the new stack's files; image pins of the new stack;
                                      generators: catalogue (yyc-ai), hermit-config, egress-config, tools-config,
                                      legacy-kube-ca (from infra/yyc-cluster-ca.pem, until A6)
   yyc/deploy/                     limits.yaml, network.yaml, cloudflared.yaml, pigeon.yaml, magpie.yaml, bridges.yaml,
                                      hermit/ (hermit.yaml, fabric.yaml in yyc-ai-fabric, juicefs.yaml with PV/PVC
                                      hermit-jfs, hermit.json, system-prompt.md, sandbox-rules.md), egress/, tools/,
                                      catalogue.json, jobs/ (the import CronJobs, `suspend: true`)
   ```
   `config.json`, `system-prompt.md` and the `jarvis-config` and `yyc-catalogue` generators stay in `yyc-ai/legacy/`
   (DESIGN 15.4). In the same commit: the ZDR guard rewrite (10.1.1); `yyc/infra/openrouter.py` allows the union of
   `legacy/config.json`'s and `egress/egress.json`'s models while both exist. The deploy creates the new objects on the
   new cluster (its plan: creates only, an empty prune set); the old cluster is not touched.
4. YYC `yyc/deploy/sealed/`: the new Sealed Secrets of platform 12.2 for ns `yyc-ai` (`pigeon`, `magpie`,
   `hermit`, `hermit-juicefs`, `egress`, `tools`, `tunnel`, `magpie-import`, `hermit-import`, `egress-import`,
   `tools-import`) and `yyc-ai-fabric/hermit-gateway`, sealed with `infra/sealed-secrets-talos.pem`; no pull Secret
   (platform R21); rows in `infra/secrets.txt`.
5. YYC tofu: 9.1 P1 block (applied by `cloudflare.yml`).
6. hermit's one-time prerequisites outside Kubernetes (`gateway-net.sh` for the new cluster's gateway node,
   `infra.sh sg|template|iam|ci-iam`, AMI bake via `hermit-ami.yml`), hermit 14.
7. The window pieces (DESIGN 9.1, 15.5). Old cluster (Flux, `yyc-ai/legacy/`): `jarvis/ui-from-bridge` (replacing
   `ui-from-yyc-ai`), `jarvis/pigeon-bridge` (selector-less → 10.0.255.2:8080) and `jarvis/ui-to-pigeon`, the
   importer's ServiceAccount, Role, RoleBinding and token Secret (B7). New cluster (`yyc-ai.yml`): `pigeon-to-ui`,
   `pigeon-from-bridge`, `import-out`'s rules; store keys `YYC_TOKEN_PIGEON_UI`, `YYC_TOKEN_MAGPIE_IMPORT_UI`,
   `YYC_PIGEON_LEGACY_VIEW_AS_KEY` sealed into `pigeon` and `magpie-import`; `YYC_OLD_CLUSTER_IMPORT_TOKEN` read once
   from the old cluster's token Secret into a file (never printed), written with `scripts/store-put.py` and sealed
   into `magpie-import`. Nothing in `jarvis` restarts with this step (new objects only). The freeze is not armed yet:
   it binds from step 8 on.
8. **The one ordinary ui rollout** (`cutover ui-rollout` and `ui-gate`, DESIGN 9.6, 15.3): a YYC commit
   (`patches/08-ui-rollout.patch`, in `yyc-ai/legacy/`) pins the `yyc-ai-ui` image of the claudecodeui commit carrying
   the router's switch module, adds `JARVIS_UI_PURGE=off`, the two pair tokens (`ui-env` re-sealed with the old cert:
   `JARVIS_UI_LU_TOKEN`, `JARVIS_UI_SWITCH_TOKEN`), `maxSurge: 2` and `spec.paused: true`, and adds
   `yyc-ai/cutover/window.json`, which arms `freeze-check` for every later commit (the check binds commits whose parent
   has the file, so this one is not checked against itself). The runner pushes it once both old pods have had nobody
   connected for 10 min; Flux applies the Deployment paused; the runner suspends Kustomization `yyc-ai` and runs
   `ui-gate` from the session, which resumes the rollout only while no old pod's CloudCLI children report running,
   held, preparing or pending work (each child's switch busy route, as `magpie import holds` reads it; connected tabs
   are not waited on, DECISIONS E5), re-checked every 2 s on each old pod not yet terminating, pausing again if work
   appears; when the rollout is
   complete it pushes `09-ui-unpause.patch` (drops `paused`) and resumes the Kustomization. The old `drain()` then has
   no work to cut. Every open tab reconnects once (DESIGN 9.6, residuals in DESIGN 14). No upper bound on the wait: Deyao is told after 48 h. From here ui and `pigeon-app` stay at
   that claudecodeui commit until A5.

#### 10.1.1 The ZDR guard, rewritten (A7.F51, A19.F21, A20.F15)

`yyc/tests/test_zdr.py` (stdlib only) and `.github/workflows/yyc-ai-zdr.yml` (push to `main` under `yyc-ai/**`,
which covers `yyc/deploy/egress/**` and `legacy/`, plus the workflow file; daily `17 3 * * *`; a failure mails the
owner as today). It checks every stack present in the tree:

| Stack | Present while | Model lists | Routes | Pinned egress source |
|---|---|---|---|---|
| new | always | `yyc/deploy/egress/egress.json`: `models[].id`, `defaultModel`, `smallModel`, `titleModel` (each must have an OpenRouter ZDR endpoint, `GET https://openrouter.ai/api/v1/endpoints/zdr`) | `routes.model.upstream` must contain `openrouter.ai` and have `model: true`; no other route may name `openrouter.ai` | the `jarvis3-chat` tag in `yyc/deploy/kustomization.yaml` `images` (a sha12), resolved to the full commit with `GET https://api.github.com/repos/DE0CH/jarvis3/commits/<sha12>` (the workflow's `GITHUB_TOKEN`; a 404 or ambiguous prefix fails) |
| old | `yyc-ai/legacy/` exists | `yyc-ai/legacy/config.json`: `models[].id`, `defaultModel`, `titleModel` | `egress.routes` as today | the remote base `github.com/DE0CH/jarvis3//deploy/chat?ref=<sha>` in `yyc-ai/legacy/kustomization.yaml` |

At each pinned commit it fetches `chat/src/egress/modelbody.ts`, `chat/src/egress/proxy.ts` and
`chat/test/egress.test.ts` from `raw.githubusercontent.com` and requires today's literals: `export const
FORCED_PROVIDER = { zdr: true, data_collection: "deny" }`; `const isModel = route.model === true;`,
`const rewrite = isModel && req.method !== "GET" && req.method !== "HEAD" ? new ModelBodyRewriter(`, `rewrite.push(chunk)`, `rewrite.end()` and `if (!rewrite) {`; `zero data retention is forced into every request body`. egress keeps those
literals and paths through its rewrite (17.5); a jarvis3 change that must move them changes this guard in the YYC
commit that bumps the pin. The guard lands in the P1 layout commit, because that commit moves the remote base the
old guard parses; its "old" half is removed in P5.4, the commit that deletes `config.json` and `legacy/`.

Side by side: everything of the old stack serves staff; the new stack serves `pigeon-test.yyc-internal.com` only.
hermit's `spareSlots` stays 0 except during the acceptance runs.

Checklist (P1 done when all pass):
- [ ] `kubectl -n jarvis get deploy` unchanged by commits 1-7 (same ReplicaSet hashes as before commit 2: no old pod
      restarted); step 8's gated rollout finished with `a1_rollout_cut_total` 0 (no running work on an old pod at
      its SIGTERM; router logs: no `stop` with a running chat) and both new pods answer `/__jarvis/sockets`;
      `kubectl -n yyc get deploy yyc-mcp` and `cronjob mcp-cleanup` unchanged.
- [ ] `kubectl -n yyc-ai get deploy`: pigeon, magpie, hermit, egress, tools, cloudflared all `2/2`;
      `kubectl -n yyc-ai-fabric get ds hermit-gateway` ready on the new cluster's gateway node (`hermit.yyc/gateway=true`),
      `wg0` up there, `bridges.sh check` green.
- [ ] tunnel `pigeon` shows 2 connectors with 4 connections each (Cloudflare API
      `GET /accounts/{acct}/cfd_tunnel/{id}/connections`).
- [ ] `yyc-ai-zdr`, `yyc-ai.yml` (`check` and `deploy`), `cloudflare.yml` and `mcp-image.yml` green on the P1 commits.
- [ ] The LU contract passes against the production old router for bot.
- [ ] `curl -sI https://pigeon-test.yyc-internal.com/` → 200 with pigeon's `aviary-request` header (pigeon's
      `server: pigeon/<sha12>` is on every response, amendment 17.2, but Cloudflare replaces `server` with its own
      `cloudflare`; `aviary-request` passes, checked 2026-11-05), the shell with `yyc-signed-out`.
- [ ] `curl -sI https://pigeon-test.yyc-internal.com/api/jarvis/me` without a token → Access 302;
      `https://ai.yyc-internal.com/` still answers from ui (no `aviary-request`).
- [ ] Component acceptance on the test host, each spec's live suite: pigeon 18.4 (sign-in as `bot@deyaochen.com`
      through the app-signin check A20.F20, view-as checks A20.F18 ported, share viewer A20.F19 ported, load timing
      A20.F21 against the baselines), hermit 24.3 (fabric acceptance: cold start, warm start, live migration with a
      live FUSE mount, host loss, packing), supervisor 22 (a turn, a background task with a CLI-started follow-up,
      an AskUserQuestion answered, a SendUserFile snapshot, an OOM guard trip ends the turn with a code), magpie 20
      (a new conversation's full life: create, turns, title, rename, share, continue, delete, restore, purge).
- [ ] A turn's model call appears in `egress.egress_log` with `person` set; the budget endpoint answers; tools calls
      reach yyc-mcp (the new namespace is admitted) and web capture.
- [ ] Rolling restart of each new Deployment (a no-op annotation bump in git, or `kubectl rollout restart`, which the
      deploy no longer undoes) during a running test turn: the turn completes, the tab's stream resumes by seq, no
      error row (each spec's zero-downtime claim, live).
- [ ] news monitor, old ops page, old sandboxes: unaffected (`/ops` on the main host still web's page).

Rollback: the switch module is inert until `move` is called, so nothing needs undoing for it (reverting step 8 is
one more ordinary ui rollout under the same quiet-moment rule); revert steps 7, 5, 4, 3 and 2 (the new-cluster
reverts carry `Deploy-Prune:` for every new object, from `yyc-apply plan --trailers`; the old-cluster ones are Flux
prunes of objects nothing uses yet); `bridges.sh down` only if the project is abandoned; the namespaces stay (empty)
until a `clusters/talos` commit removes them; leave the databases.

### 10.2 P2 (A2): bulk import and rehearsals (still dark for staff)

The one-shot Jobs of this phase and of A6 (`hermit-jfs-owner`, `magpie-import-plan`, the bulk import and its re-runs,
`magpie import holds`, render parity) live in `yyc/oneshot/` and run with `yyc/oneshot/run.sh <file>
[--replace]` on the images `yyc/deploy/kustomization.yaml` pins at that moment, never through `yyc-ai.yml`: a Job's
pod template is immutable, so a Job in the applied folder refused every re-pin of the tags until a commit named it in
`Deploy-Replace`/`Deploy-Prune` (2026-11-05). Before `preflight` and `start-handover` (which read the plan Job,
`config.json` `planJob`), `run.sh import-plan.yaml --replace` runs the plan on the current images; `oneshot/` is
deleted with the runner in A6.

Steps:
1. Job `hermit-jfs-owner` (hermit 15.4), then the CronJobs `hermit-import`, `egress-import`, `tools-import`
   unsuspended in git (each first run is the full copy).
2. Job `magpie-import-plan` → review: 0 id conflicts, 0 lines over the cap, 0 auto-memory files, the census numbers
   of section 4.
3. Job `magpie-import` (`run`, then `verify --all`): the bulk import, ≤ 10 min.
4. `magpie import holds` (a one-shot Job): its running sets match what CloudCLI shows for the same moment (bot starts
   a turn on the main host; the set lists that conversation within 5 s and drops it within 5 s of the turn's end),
   which proves `holds.js` works against the live ui image.
5. `cutover start-handover`: `patches/05-import-deployment.patch` lands now, at the end of A2's import steps and before
   the rehearsals, not at A3: upload ingestion (7.4.3), the per-person gate of R1, the reverse hand-over of R3 and the
   writer sidecar's export of rehearsal 10 are the Deployment's own work, so A2 cannot pass without it. It changes
   nothing staff see: before the gate the loop only syncs (catch-up passes, upload ingestion, the share and view-as
   passes, the running mirror, warm hints), reads the ui pods' reports and running sets read-only (as step 4's holds
   Job did), and releases nothing (7.5: releases need the gate, and R1's is per person, bot only); staff stay on the
   old stack and pigeon serves only `pigeon-test` until the flip. Upload ingestion running (every person's
   `cloudcli-assets/` in `import_uploads`) is this step's check. No socket measurement is needed: the move does not
   wait for tabs. From here the Deployment keeps the import current, so no `catch-up` Job is run (rehearsal 7 waits
   for a pass instead); a fix to the importer ships as a re-pin of the magpie image, which rolls the Deployment.
6. Rehearsals (below).

Rehearsals (all on the test host, all by `bot@deyaochen.com` unless said):
6. **Render parity.** For every imported conversation: the old side's projection (run `parity.mjs`, a copy of
   `paseo-timeline.ts`'s projection driven from the transcript, inside a ui pod as A20.F17 does) and the new side's
   (`magpie import parity <id>`: magpie's timeline rows through pigeon's Row → PaseoRow mapping) normalised to
   `[kind, role, text sha, tool name, tool status, attachment names]` per row. The only differences allowed are the
   display fixes of magpie 19.3 and its intended differences (each listed by row); anything else fails the step. A conversation repaints once at
   its release (DESIGN 9.3), so the gate is that magpie's rows carry the same content, not that the old in-memory
   timeline is reproduced.
7. **Continue an imported conversation.** Make two probe conversations on the old system as bot (`ui-pod-chat`,
   A20.F17): one plain, one with an image upload, a SendUserFile and a background task. Wait for the hand-over loop's
   next pass (`magpie import status`: both imported, verified). Release
   them with `magpie import release <id> --force` (bot is a test identity and stops using those conversations on the
   old side). On the test host: open each, send a turn. The supervisor's `claude --resume` reads the imported
   transcript through FUSE; the reply refers to the earlier turns; the image still renders; `magpie import verify
   <id>` of the frozen source still matches.
8. **Flip mechanism (R0).** First: 20 WebSockets on `pigeon-test`, opened while its record points at tunnel `yyc-ai`,
   held across the record change to tunnel `pigeon` for 10 min: zero closes (else the ingress-rule variant of 9.3, the
   same way). Then: Point `pigeon_test`'s DNS record at the old tunnel (tofu commit), open a WebSocket on
   `pigeon-test` (served by ui), start a long turn there; flip the record back to tunnel `pigeon` (tofu commit).
   Measure: seconds from the apply until 20 consecutive `HEAD /` probes (1 per second) carry pigeon's `aviary-request` header; the
   open socket keeps streaming until the turn ends. Note: in this rehearsal the turn runs on the old stack and stays
   there (its conversation is bot's probe); the point is the edge's behaviour.
9. **Every state at once (R1, DESIGN 9.9).** Bot holds a long old turn in A, a hold in B, an ask in C, an idle
   warm old sandbox in D, a file attached but not sent in E, a draft typed but not sent in F, a web tab on A and F, the
   iOS phone replica on B and an admin's view-as tab of bot; a send is in flight at the move. `cutover move --person
   bot@deyaochen.com` (it arms the 503 and the pass-on for bot alone, never the bare `move` that arms every person:
   before the flip that would refuse every staff WebSocket while the DNS still points at the old tunnel; the runner
   refuses `--person` against a router whose report has no `armedPersons`; the ui runs claudecodeui e973aa24, whose
   `move?person=` arms only that person, rolled out by the `ui-fix` recorded exception, decision 16), then the per-person gate (`magpie import allow-handover --person bot@deyaochen.com`); screen
   recording of every tab plus the counters: every zero counter stays 0; each tab reconnects once; A streams to its
   end on the old stack and hands over; E's file reaches the agent; F's draft is still in the composer after the move
   and after A's release repaint; the in-flight send is accepted once; view-as continues; C ends by Stop; a release
   happens while a tab shows the conversation and the next turn there runs on magpie. R3: a reverse hand-over and
   forward again (14.2), with a tab showing the conversation throughout: at most one repaint.
10. **Rollback export.** For the conversation continued in step 7: the writer sidecar's export (`--since <step 7
    time>`) against the fenced copy; open it on the old system as bot: the new turns render and a further turn on the
    old system resumes from them. Then delete the probe conversations on both sides.
11. **Load (R4).** 10 parallel synthetic conversations on the test host (the A20.F22 benchmark prompt) during R1's
    hand-over: every zero counter 0, fence p99 ≤ 300 ms, magpie p99 latencies within magpie 17's targets. There is no
    R5 (decision 12).

Checklist (P2 done when all pass):
- [ ] `magpie import status`: `imported` = the number of sessions rows in `cloudcli` (all schemas), `stuck` = 0,
      `errors` empty.
- [ ] `verify --all` green; census: blob count and bytes within 5 % of section 4's estimate (453 images).
- [ ] Shares: every live share opens on the test host (`/s/<id>`), its workspace files download (clone present),
      revoked ones answer 404.
- [ ] view-as log: 39 rows (plus any new) visible on hermit's ops page through magpie.
- [ ] egress budget on the new egress equals the old egress's (same `since`) within USD 0.10, 2 minutes after the
      CronJob's run.
- [ ] Step 4 (holds), step 5 and rehearsals 6-11 passed, with the numbers recorded (flip propagation seconds,
      socket survival over 10 min, move time, fence times, counters).

Rollback: `Cutover-Revert: 05-import-deployment` (the hand-over Deployment gone, nothing released: every imported
conversation is still migrating), then `magpie import reset --yes` (magpie's imported conversations are gone; native
test conversations stay); CronJobs back to suspended. Staff saw nothing beyond A1's one ordinary rollout.

### 10.3 A3-A5: the flip, the hand-over, retiring the old stack (section 11 is the runbook)

Side by side: from T0 new connections reach pigeon; minutes later the move brings every open tab to pigeon and the
old connectors go to 0; the old children keep serving legacy conversations through LU; idle conversations move to
magpie at the gate, the rest as their old turns end; from A5 only the new stack serves, with the old stack at zero
but present until P4 ends.

Checklist (A5 done when all pass):
- [ ] Every zero counter of DESIGN 9.9 read 0 from T0 − 5 min to the end of A5; `magpie_fence_seconds` p99 ≤ 0.3 s
      and max ≤ 3 s; reconnects per tab at the move ≤ 1; no edge 5xx for the host except pigeon's own (Cloudflare analytics `edgeResponseStatus`, 1-minute buckets) and no tunnel error 1033/502.
- [ ] `magpie import status`: every conversation owned, `fenced: 0`, `stuck: 0`.
- [ ] `verify --all` green: every released conversation's source files are byte-identical to what was recorded at
      its release (no write after the hand-over).
- [ ] The old connectors at 0 since the move; the preconditions of `retire-old` held 30 min; then `kubectl -n jarvis
      get pods -l 'app in (ui,cloudflared)'` empty; `jarvis.sandboxes` non-gone = 0; old web, egress and tools at 0.
- [ ] Live probes as bot on the main host (runner `probe`): sign-in (web and app-signin), list, open an imported
      conversation, send a turn in it, new conversation, upload, SendUserFile, share and open the share, view-as by an
      admin, `/ops` (hermit's page through pigeon), the news dashboard's YYC AI panel shows hermit's snapshot,
      yyc-mcp still admits a staff token.
- [ ] iOS: the CI phone replica (`yyc-ios` workflow's Maestro smoke, A20.F16, extended by a signed-in flow with
      bot's token from the app-signin check) passes against `ai.yyc-internal.com` with the shipped build's source
      commit.
- [ ] Error budget for the first working day: no `sandbox.*` or `internal` turn failure that is not explained by an
      intended test; pigeon `pigeon_frames_dropped_total` flat; magpie `reconstruct mismatch` 0.

Rollback: section 14.
### 10.4 P4: after A5, no wait

State: old stack present at 0 replicas (ui, old connectors, web, old egress, old tools); old fabric with 0 hosts
(`aws.maxHosts` unchanged, no demand); the bridge kept for a rollback (DESIGN 9.10): Deployment `magpie-import` with its
writer sidecar, NetworkPolicies `jarvis/ui-from-bridge`, the egress copy; ui at 0 keeps its image with the switch
module, so a scale-up for a rollback needs no other change.

P4 has no soak time (Deyao, 2026-11-07: "Stop waiting"; the 72 h was the plan's own margin, not his rule). Its checks
are read once, right after A5 (P4 done when all pass):
- [ ] No open defect of severity "staff lost data or could not work"; every staff-visible error explained.
- [ ] Since A5: `pigeon`, `magpie`, `hermit`, `egress` restarts 0 unplanned; magpie `cc write` p99 within magpie 17;
      sandbox start p50/p95 within A20.F22's baselines.
- [ ] Budget and spend on the new egress match OpenRouter's own usage for the key within USD 0.50/day.
- [ ] Deyao has not asked to roll back.

The zero-downtime proof under real load is every later rolling update of a new component (forward fixes only), not a
P4 gate. Rollback in P4: section 14.1 (old stack back up, then per-conversation reverse hand-over). After P4:
`cutover import-done`, then forward fixes only.

### 10.5 P5 (A6): delete

Order matters (each step's check before the next):
1. **Final copies.** `egress-import`, `tools-import` and `hermit-import` (`--delta`) last runs; check counts equal
   the sources'.
2. **Final dumps and the archive (decision 5).** Run the daily dump job once by hand (`kubectl create job
   --from=cronjob/pg-dump-daily`) so that `jarvis` and `cloudcli` have a dump of their final state under the 30-day
   Object Lock; record the object keys. Then the old chat UI's archive (15.4): a separate dump of `jarvis`'s legacy
   tables and a copy of the bucket prefix `yyc-ai-files/jarvis/files/` under `yyc-pg-backups/archive/` (kept, not
   expired), checked by listing and a test restore of the dump into a scratch database; then the prefix is deleted.
3. **Old fabric.** hermit's `legacy-down.sh` (hermit 22.3 step 5): terminate EC2 instances tagged
   `app=yyc-ai-sandbox`; on `yyc-pool-apps-worker2`: stop and disable `sandboxd` and `sbx-jfs`, delete `wg0`, nft
   table `ip sbxhost`, the ip rules and table 51822, `/etc/sbx`, `/var/lib/sbx`; Hetzner route 10.200.0.0/15 and
   firewall `yyc-sbx-gw`; launch template `yyc-ai-sandbox-host`, its AMIs and snapshots; IAM users
   `yyc-ai-sandbox-scaler`, `yyc-ai-sandbox-ami-ci`; security group `yyc-ai-sandbox-host`. Then Job
   `hermit-jfs-owner` again (hermit 15.4).
4. **Cluster (one YYC commit, planned from the session first with `yyc-apply plan --trailers`):** `legacy` leaves
   `resources` of `yyc/deploy/kustomization.yaml`; `legacy/` is deleted. The commit carries
   `Deploy-Prune-Namespace: jarvis` and `Deploy-Prune:` lines for PV `yyc-ai-jfs`, RuntimeClass `gvisor`,
   `kube-system/sbx-overlay-route` and every other removed object; the deploy deletes, after its rollout check,
   everything in namespace `jarvis` (the importer's Role and RoleBinding and `jarvis/ui-from-bridge`
   included), the PV `yyc-ai-jfs` (Retain: the volume is untouched), `kube-system/sbx-overlay-route`,
   RuntimeClass `gvisor`, the old Sealed Secrets (`yyc/deploy/sealed/`: `ui-env`, `ui-juicefs`, `web-env`,
   `sandbox-vm`, old `egress-env`, `tools-env`, `tunnel`, `ghcr-pull` in `jarvis`), the import CronJobs (`jobs/` deleted) and Secrets `magpie-import`, `hermit-import`, `egress-import`,
   `tools-import`, and NetworkPolicy `magpie-import-egress`. Then a `clusters/hetzner` commit removes Namespace
   `jarvis`, the fenced applier's `jarvis` RoleBinding and the deployer's `jarvis` RoleBinding, `kube-system` Role
   and `gvisor`/`yyc-ai-jfs` names (Flux `yyc-apps` deletes the now-empty namespace). In the same commit: `config.json` and the top-level `system-prompt.md`
   deleted with the `jarvis-config` generator (their content lives in `hermit/`, `egress/` and `catalogue.json`);
   the ZDR guard's old half and `openrouter.py`'s union removed (10.1.1); `.github/workflows/yyc-ai-ami.yml`
   deleted (replaced by `hermit-ami.yml`); `monitor-keys` already lost `JARVIS_OPS_TOKEN` in P3. Nothing that reads
   the catalogue changes: every reader moved in the switch commit (D15, D16; hermit 21.5), so yyc-mcp, `mcp-cleanup`
   and the `yyc-catalogue` ConfigMap are untouched by this commit.
5. **Cloudflare (one tofu commit):** 9.1 P5 block. Then delete Deyao's hand-set CNAME `ai-yyc-ui.deyaochen.com` in
   his zone (Cloudflare API with his account token) and drop it from `monitor.py` `SAAS_NAMES` and
   `news-stream/deploy/README.md`'s record list.
6. **Databases (YYC `news-stream/deploy/db`, prune off):** `Database` CRs `jarvis` and `cloudcli` get
   `ensure: absent` and `databaseReclaimPolicy: delete`; managed roles `jarvis`, `cloudcli` `ensure: absent`;
   `pg-dump-daily` loses them; `pg-role-jarvis`, `pg-role-cloudcli` Sealed Secrets deleted. After CNPG reports both
   databases gone, the CRs are removed from git and deleted with `kubectl` (the Kustomization does not prune by
   design). The old chat UI's tables were archived in step 2 (decision 5).
7. **Contraction release (window code):** pigeon drops LU, tickets, the copy-at-send of attachments into the old
   child and the legacy view-as key (the legacy-path resolution stays until A7); then magpie drops
   `conversations.migrating` and `legacy_fence_at`, `import_sources`, `import_shares`, `import_control`,
   `legacy_tickets`, the 409 path, the fenced wait, the "skip migrating"
   checks and the `magpie import` subcommands with the `magpie-import` crate. Expand/contract order: pigeon first
   (stops calling it), then magpie.
8. **Code (section 15.2):** deletions in claudecodeui (the router with its switch module), jarvis3, aviary (hermit's
   `import-jarvis`, the LU contract rig, legacy-in-a-box), YYC (`yyc-ai/cutover/`, `infra/gvisor-node.sh`, README
   sections), claude-env (skill `yyc-ai` rewritten, lessons).
9. **GHCR:** delete package `yyc-ai-ui` (and `jarvis3-sandbox` once no AMI or host uses it) (`github-web` skill for
   package deletion if the API refuses).

Checklist (P5 done when all pass):
- [ ] `kubectl get ns jarvis` → NotFound; `kubectl get pv yyc-ai-jfs` → NotFound; JuiceFS `ws/` intact
      (`juicefs info /ws` counts unchanged from before step 4).
- [ ] AWS: no instance, template, AMI, snapshot, security group or IAM user of the old fabric
      (`aws ec2 describe-instances --filters Name=tag:app,Values=yyc-ai-sandbox` empty, likewise for the others).
- [ ] Hetzner: route 10.200.0.0/15 and firewall `yyc-sbx-gw` gone; `yyc-pool-apps-worker2` has no `wg0`, no
      `sandboxd` unit.
- [ ] Cloudflare: tunnel `yyc-ai` deleted; `staff_ai` destinations are exactly `ai.yyc-internal.com/{api,paseo-ws,ops}`;
      no record or custom hostname for `pigeon-test` or `ai-yyc-ui`; `ai.yyc.deyaochen.com` still 301s.
- [ ] Postgres: `\l` shows no `jarvis`, `cloudcli`; the final dumps exist in `yyc-pg-backups`; the archive of the old
      chat UI exists under `yyc-pg-backups/archive/` and is excluded from expiry; `yyc-ai-files/jarvis/` is empty.
- [ ] `git grep -nE '\b(ui|web)\b'` over YYC `yyc-ai/`, aviary and jarvis3 `deploy/` finds no YYC AI component of
      those names (allow-list for unrelated words reviewed by hand); `git grep -n 'JARVIS_CC_TOKEN\|JARVIS_SENT_FILES\|cloudcli\|/internal/cc'`
      in YYC and aviary is empty; jarvis3 keeps only egress and tools references.
- [ ] Store `yyc`: keys only the old stack used are removed (`YYC_JARVIS_CC_TOKEN`, `YYC_JARVIS_SENT_FILES_TOKEN`,
      `YYC_CLOUDCLI_DATABASE_URL`, `YYC_CLOUDCLI_JWT_SECRET`, `YYC_PG_CLOUDCLI_PASSWORD`, `YYC_JARVIS_DATABASE_URL`,
      `YYC_PG_JARVIS_PASSWORD`, `YYC_SBX_*`, `YYC_CF_AI_TUNNEL_TOKEN`, `YYC_SANDBOX_GHCR_DOCKERCONFIGJSON`,
      `YYC_AI_AMI_AWS_*`, the old AWS scaler key, `YYC_TOKEN_PIGEON_UI`, `YYC_TOKEN_MAGPIE_IMPORT_UI`,
      `YYC_PIGEON_LEGACY_VIEW_AS_KEY`; platform 12.3), each with `stores.txt` updated.
      `YYC_JUICEFS_VOLUME_NAME`, `_META_URL`, `_CSI_ENVS` stay (Secret `hermit-juicefs` uses them, platform 12.2).
- [ ] `git grep -n 'x-aviary-lu\|legacy_tickets\|legacy_fence'` in aviary finds nothing; only the A7 legacy-path
      code (`import_uploads`, `uploads.legacy_path`) remains.
- [ ] `yyc-ai-zdr` green on the P5.4 commit (new half only); `kubectl -n yyc get cm yyc-catalogue` has key
      `config.json` with the same content as before P5.4, and no yyc-mcp Pod or `mcp-cleanup` run changed.
- [ ] All CI green after the deletions (aviary, jarvis3, YYC, claudecodeui `jarvis4-runtime.yml` local-mode tests
      included: Jarvis 4's local mode is untouched).
- [ ] Live probes (the A3-A5 list) pass again after the cluster commit.

Rollback: none needed for deletion of drained, unused objects; a mistaken deletion of a live object is a forward fix
(the new stack does not reference anything in `legacy/`, which the P1 render check and P4 with ui at 0
already prove).

### 10.6 P6: 30 days after A5

- `hermit legacy-cleanup --confirm` (hermit 22.3 step 6): removes `users/` and `sent/` from JuiceFS, and empty
  `ws/` folders without a conversation in magpie.
- The final dumps of `jarvis` and `cloudcli` reach the end of their Object Lock and expire with the normal lifecycle.
- Check: `juicefs summary /` shows only `ws/`; the bucket prefix `yyc-ai/` shrank by the old `users/`+`sent/` bytes
  (about 317 MB) after JuiceFS's background deletion.

### 10.7 A7: legacy-path cleanup (A5 + 90 days)

Drafts keep attachments with old asset paths for the 720 h Access session lifetime and longer. So the legacy-path set
lives on after A6: `import_uploads`, `uploads.legacy_path` and their resolution in M3/M7. A7 runs once 90 days have
passed since A5 **and** magpie has seen no old asset path for 30 days (`magpie_legacy_path_total`): a contraction
release drops them; `pigeon-app` drops the old-build chunks 30 days after the last old-build tab was seen. Check:
`git grep -n 'legacy/\|x-aviary-lu\|import_uploads\|legacy_fence'` in aviary empty outside history.

---

## 11. The switch day (the `cutover` runner)

### 11.1 The runner

`YYC/yyc-ai/cutover/` (Rust, binary `cutover`, deleted in A6). It runs in a Jarvis 4 session holding store `yyc`,
with the YYC repo checked out, `kubectl` for exec and reads on both clusters (the old one through `cluster.sh ssh`,
the new one through its operator path, DESIGN 15.2 N2), `gh` with read access to
workflow runs, and the Cloudflare analytics token. Each subcommand is resumable: it records its progress in
`~/artifacts/cutover/state.json` (step, times, commit shas) and re-running it continues from the last finished step;
a commit step is skipped when its commit is already on `main`. Prepared commits are patch files in
`yyc-ai/cutover/patches/` (made and reviewed in A2), applied with `git am`, pushed straight to `main` (rebase on
reject). After a push that changes the new cluster the runner waits until `yyc-deploy/inventory-yyc-ai`'s `commit` is
that commit or a descendant with phase `done` and that `yyc-ai.yml` run is green (`gh run list -w yyc-ai.yml
--json`); after a push that changes the old cluster (`yyc-ai/legacy/`, `news-stream/deploy/apps/`, `db/`) it runs
`flux reconcile kustomization <name> --with-source` through `cluster.sh ssh` and waits until the Kustomization is
Ready at that revision (DESIGN 15.3). Only the switch commit (`10-flip`) changes both, and `flip` waits on both (23).

| Subcommand | Does | Fails when |
|---|---|---|
| `freeze-check` | the window freeze (DESIGN 9.6): run by `yyc-ai.yml`'s `check` job on every commit whose parent has `yyc-ai/cutover/window.json` (added by `08-ui-rollout`, removed by `import-done`); refuses a diff that touches `jarvis-config`, a `yyc/secrets-rev` annotation in `yyc-ai/legacy/`, `ui.yaml`, `db.yaml` or the old connectors' manifests (except the runner's own `09-`/`10-`/`20-`/`30-`/`40-` patches), either cluster's node configuration (A3 to A5), or (A3 to A5) `egress.json` `budget`/`models` and `catalogue.json` users/admins | any of those |
| `ui-rollout` | A1 step 8 (10.1): pushes `08-ui-rollout.patch` (Flux applies the Deployment paused), waits until both old ui pods have had no client connection and no child for 10 min, suspends Kustomization `yyc-ai`, runs `ui-gate`, then pushes `09-ui-unpause.patch` and resumes the Kustomization; checks both new pods answer `/__jarvis/sockets` | nobody-connected not reached in 48 h (Deyao told; keeps waiting) |
| `ui-gate` | run by `ui-rollout` from the session against the old cluster, with Kustomization `yyc-ai` suspended: waits until no old pod has had running work for 10 min (by exec, every CloudCLI child's switch busy route: running, processes, preparing, pending; a child that cannot tell counts as busy; connected tabs are not waited on, DECISIONS E5); re-checks, resumes; every 2 s re-checks each old pod not yet terminating and pauses again if work appears; records the running work seen at each SIGTERM (`a1_rollout_cut_total`) | — (it waits; the run stays in progress) |
| `preflight` | the 11.2 list | any item fails (prints which) |
| `start-handover` | applies `patches/05-import-deployment.patch` (Deployment `magpie-import`, 7.1), pushes, waits for 2/2 ready and the status `phase: sync` with the first (catch-up) pass done and `stuck: 0` | not ready in 10 min; `plan` blockers |
| `flip` | applies `patches/10-flip.patch`, **the switch commit** (D15): the tofu change of 9.1 A3 (DNS of `origin_ai`, `pigeon-test` removed, `access.tf` reading `catalogue.json`; `ws` and `__jarvis` stay on both tunnels until A6), every catalogue reader and text of hermit 21.5, the monitor of hermit 21.4; pushes; waits for the `cloudflare.yml` run and the `yyc-ai.yml` run of the commit to succeed, and Kustomization `yyc-apps` Ready at the new revision (`monitor.yaml`); then probes `HEAD https://ai.yyc-internal.com/` from an HK vantage once a second until 20 consecutive answers carry pigeon's `aviary-request` header (the edge replaces `server`; timeout 5 min); then waits the settle time (10 min with every probe agreeing) | workflow red; timeout |
| `move [--person <email>]` | first `POST /__jarvis/move` without a person on every ui pod (enumerated from the API), which arms the 503 for new non-LU upgrades and the HTTP pass-on to pigeon; then every 10 s, for each person with no warm call and no upload at pigeon in the last 60 s (`/internal/switch/activity`), `POST /__jarvis/move?person=`: each non-LU socket is closed at an instant with no outstanding request and ≥ 5 s without an inbound frame (DESIGN 9.3); keeps running until the gate is set | a socket stays busy, opaque or with an active person for 10 min (reported with its person; Deyao told; nothing else waits on it) |
| `old-connectors-0` | once every pod reported 0 non-LU sockets and 0 in-flight non-LU HTTP for 60 s: applies `patches/20-old-connectors-0.patch` (Deployment `jarvis/cloudflared` to 0), pushes, waits for 0 pods (with `move` still running), records `connectors_off_at` | a pod lingers past its 200 s grace |
| `allow-handover [--person <email>]` | the gate (DESIGN 9.2); `--person` only for rehearsal R1 | a ui report is stale or shows a non-LU socket or in-flight non-LU HTTP |
| `watch` | prints every 5 s: conversations per state and what each waits for (`in use`, `hold`, `old sandbox warm`, with the owner; an open question holds nothing, it is carried over), LU sockets, opaque or never-quiet old sockets with their person, the zero counters of DESIGN 9.9; any non-zero counter holds new fences (`hold on`) and Discords Deyao; alarms on `preparing` p99 above the A2 baseline, a ui pod restart, the router's "busy"/"evicted" lines | — |
| `retire-old` | A5: checks the preconditions of DESIGN 9.7 A5 continuously for 30 min, then applies `patches/30-ui-0.patch` and `40-old-backends-0.patch` (old web, egress, tools 0), one push and wait each; the old fabric's gateway stays | a precondition breaks (restarts the 30 min) |
| `import-done` | after P4: `magpie import verify --all` (one-shot), then `patches/50-import-done.patch` (new cluster: Deployment `magpie-import`, the `yyc-ai` egress rules to the bridges, `ui-legacy`, with `Deploy-Prune:` trailers) and `patches/51-import-done-old.patch` (old cluster: `jarvis/ui-from-bridge`, `ui-bridge`, the importer's ServiceAccount, Role and token; Flux prunes), pushes each and waits on its cluster; removes `window.json` | verify red |
| `probe` | the live probes of the A3-A5 checklist (10.3) as bot | any probe fails |
| `rollback <stage>` | section 14 | — |
| `status` | prints state.json and the live facts | — |

### 11.2 Preflight

- [ ] A2's rehearsals passed (no time window: the switch runs whenever it is ready, D5).
- [ ] Job `magpie-import-plan` green: 0 id conflicts, 0 lines over the cap; memory files reported (D18 of DESIGN).
- [ ] Every new Deployment ready with 2+ pods; tunnel `pigeon` has 2 connectors; `pigeon-test` probes pass.
- [ ] Both ui pods answer `/__jarvis/sockets` with `opaque` 0; pigeon answers `/internal/switch/activity`; the freeze
      intact (no ui or old connector restart since the A1 rollout; no node drain or `pg` restart planned for the day).
- [ ] The persons whose newest Access token expires before T0 + 8 h are listed in the state file (DESIGN 9.5); the
      flip patch pins pigeon's HPA and no pigeon rollout is pending.
- [ ] `import_uploads` caught up (every file under each person's `cloudcli-assets/` ingested); warm hints flowing.
- [ ] `pigeon-app` holds every chunk of every ui image served in the last 720 h (D13), including the A1 image's.
- [ ] New egress budget headroom ≥ USD 10; `egress-import` ran in the last 2 minutes (8.3).
- [ ] Relay: kept → a relay token made on the new stack's page answers a model call through `ai-relay`; dropped →
      every person with a live `relay_tokens` row has been told it stops at A5.
- [ ] Label store: from a `pigeon-test` sandbox, the label store's REST API lists the designs and
      `GET …/labels/<guest>.png` returns a label image (WP-61).
- [ ] AWS: vCPU headroom for both fabrics' peak (`aws service-quotas`).
- [ ] The last `yyc-ai.yml` run green with an empty plan; no Flux Kustomization not Ready on the old cluster.
- [ ] `bridges.sh check` green (every bridge target with a callee pod healthy); rehearsal R6 passed; the old
      API answers the importer (B7); `window.json` has `"nodeWork": "hold"`, the Talos upgrade workflow's and
      `upgrade-k8s`'s dry runs skip every node, and the Descheduler CronJob is suspended (the same `preflight` commit;
      DESIGN 15.2 N8).
- [ ] `catalogue.json`'s users and admins equal `config.json`'s (the CI step) on the commit to be flipped.
- [ ] Patches apply cleanly on `main` (`git am --check`); `10-flip.patch`'s tree passes the `check` job's steps,
      `yyc-ai-zdr` and the `mcp-image.yml` smoke test in a local dry run.
- [ ] The hand-over Deployment (started in A2 step 5) green: `magpie import status` shows `phase: sync`, `stuck: 0`,
      `imported` = sessions rows now, no gate set (R1's per-person gate cleared with the rehearsal's probes).

### 11.3 Order of the day

| Step | What staff see |
|---|---|
| `preflight` (`start-handover` ran in A2 step 5) | nothing |
| T0: `flip`, settle | nothing: new page loads and new requests reach pigeon (legacy conversations through LU from the same old child); open sockets stay on ui; the dashboard reads hermit's snapshot |
| `move`, `old-connectors-0` | each open tab shows its ~2 s reconnect notice once (a person typing, once they pause for 30 s) and resumes on pigeon with the same view; running turns keep streaming |
| `allow-handover` (A4, automatic) | a shown idle conversation repaints once at its release; conversations with a running old turn release after it ends |
| A5: `retire-old` | nothing: nothing is connected to what is scaled down |
| P4, then `import-done` | nothing |

Expected length: about 15 min from `flip` to the end of `move`; A4 as long as the longest running old turn plus its
old sandbox's 30 min idle release; A5's 30 min of preconditions. One working afternoon is the expectation, not a
bound: a legacy conversation in continuous use, or an old ask nobody stops (it waits for Stop or the old sandbox's
8 h lifetime), holds A5 until it ends; `watch` names it with its owner (DESIGN 9.4, 9.7).

---

## 12. Running chats and open tabs during the window

Text of record: DESIGN 9.3-9.5. In every row nothing is read-only, cut or lost; what the person may see is the one
reconnect of the move and one repaint at a release (decision 11).

| Situation | What happens |
|---|---|
| Web tab open, socket on ui at T0 | stays on the old path until the move (minutes); then closed at a request-free instant, shows the ~2 s reconnect notice, reconnects to pigeon with its cursors valid (the same old child serves the conversation until its release) |
| iOS app or a WebKit tab, socket on ui | moved the same way; a backgrounded app finds its socket closed and reconnects on its next foreground |
| A new tab after T0 | a native socket: legacy conversations through LU, owned ones native; creates are native |
| A turn running in a CloudCLI child | finishes on the old stack; the move closes only client sockets, so the turn keeps running and every tab sees it live through LU; Stop works; the conversation is released after it ends |
| A turn in the post-turn hold | as above until the hold ends (≤ 30 min) |
| An AskUserQuestion / ExitPlanMode open in an old turn | the old runtime parks it once the turn waits only on it (claudecodeui fd1b3ed5, DECISIONS 17): the question is kept in `jarvis_open_questions` and the sandbox idles out; the hand-over carries an unanswered question over as a native open question of a parked turn keyed by its `tool_use_id` (magpie-import `questions.rs`, magpie 8.1a), and the person's answer on the new stack resumes the conversation; an answered one whose turn has not run keeps the conversation legacy until it runs (`holds.js` counts it running); Stop from any tab ends it |
| A sandbox still preparing on the old side | as a running turn |
| A send in flight at the move | the socket is not closed until it is answered, and not within 5 s of any inbound frame or while the person typed or attached in the last 30 s (DESIGN 9.3) |
| An old ask nobody stops, or a legacy conversation in continuous use | stays legacy, fully working through LU; it holds A5 (not the others) until it ends; `watch` names it with its owner |
| A stale edge location still routing to the old tunnel after the move | a new upgrade gets 503 before the 101 and the app retries to pigeon; an HTTP request (an upload) is passed on to pigeon unchanged |
| A send that arrives during a fence | waits ≤ 3 s in the ticket call and becomes the first native turn |
| A draft in the composer | survives the move and the release repaint (client state the replacement does not touch; a persona-rig assertion) |
| An attachment | before T0: resolves through `import_uploads` wherever it is sent (synchronous ingest on a miss); T0 to the gate: written into the old child through LU and recorded in magpie, so its old path works on the old socket, through LU and natively; from the gate: stored in magpie under the old route's limits and copied into the old child under the send's ticket for a legacy conversation (DESIGN 9.5) |
| A person whose Access session ends during the window | on an LU 401 pigeon answers with its signed-out contract (the sign-in prompt, as at the edge), never a write error; `lu_401_total` alerts |
| A shown conversation at its release | one `agent.timeline.replacement`: the tab refetches the tail in magpie's epoch, as after any ui deploy |
| A view-as tab opened on the old router | keeps working: pigeon verifies the old token with the derived legacy key and re-signs LU calls with it |
| A share page `/s/<id>` | routed by where the share lives |
| Deep links `/h/srv_conversations/workspace/wks_<id>` | ids unchanged |
| A sign-in in progress | the callback lands on pigeon, which implements the same contract |
| `/ops` | pigeon forwards to hermit's operator page (admins see the new page) |
| News dashboard YYC AI panel | reads web's snapshot until the switch commit rolls the monitor, then hermit's (same fields, hermit 18.3) |
| yyc-mcp | the switch commit changes only the source file of ConfigMap `yyc/yyc-catalogue`, not its key; the kubelet swaps the content in place |

---

## 13. Version skew

### 13.1 iOS builds

- Nothing in the app changes for the switch and no build is released for it. Shipped builds are frozen on: the
  origin `ai.yyc-internal.com` (the `YYC_SITE` build secret), `/paseo-ws`, `GET /api/jarvis/app-signin?state=` and
  its `dev.de0ch.yycai://signed-in?state=…#token=…` redirect, the `cf-access-token` header plus the
  `CF_Authorization` cookie, `serverId` `srv_conversations`, the workspace and agent id mapping, the FEATURES flags,
  the REST paths of A15.F14, and the zod-aot validators compiled into the bundle. pigeon keeps all of them
  (pigeon 7, 5).
- Supported clients file: `aviary/testing/paseo-contract/clients.txt` (pigeon 18.1) lists the claudecodeui commits of
  every client that may be in use: **every ui image commit served within the 720 h Access session lifetime** (from
  the ui pin history in DE0CH/YYC's git log) and the source
  commit of each `yyc-ios` run uploaded to TestFlight since 2026-10-22 (read from the workflow runs' `head_sha`).
  pigeon's contract suite (pigeon 18.1) checks out each commit's `paseo/packages/protocol` and validates every frame
  pigeon can emit against that commit's `WSOutboundMessageSchema` and response schemas: a new enum value, message type
  or required-field change that an old client would reject fails pigeon's CI. A client leaves the file when no
  `/paseo-ws` connection with its version has been seen for 30 days.
- Builds that baked the old host `ai.yyc.deyaochen.com` (if any exist) already fail today (the 301 Worker answers the
  WebSocket with a 301); the switch neither fixes nor worsens them.
- `hello.rejected incompatible_protocol` (pigeon 7.2) is the lever for a future forced upgrade; it is not used at the
  switch.

### 13.2 Web tabs

- A tab loaded from ui keeps running ui's JavaScript after it reaches pigeon. Its lazy chunks are fetched by hashed
  name; pigeon serves them because `pigeon-app` is seeded with the static chunks of **every** ui image served within
  the 720 h Access lifetime (D13) and keeps old-build chunks for 30 days after the last old-build tab is seen (pigeon counts `appVersion`
  from `hello`). The chunks are copied from the images themselves, so no build-determinism gate is needed; ui and
  `pigeon-app` stay at the A1 commit until A5 (DESIGN 9.6).
- `index.html` is `no-store`; the tab's next reload gets pigeon's build. `/sw.js` (the old service worker's kill
  switch) is served signed in and signed out (pigeon 5.2.1).
- Any change pigeon makes to its frames must pass the contract suite for every listed ui commit (13.1).

### 13.3 Between the new components

Each component's own rollout rules apply (expand-only migrations within a release; pair tokens with overlap; the
supervisor reports its version, supervisor 21.3). The pigeon and magpie releases carrying the window code ship in A1
(magpie first: pigeon calls its new routes); their removal in A6 and A7 goes pigeon first, then magpie. During A3-A4 the
runner holds every pigeon rollout and pigeon's HPA is pinned (`minReplicas = maxReplicas`, in the switch commit,
reset in `30-ui-0`), so nothing but the move reconnects a tab and LU reopens only behind a reconnect through the edge
(DESIGN 9.3). Token-level live frames play no part
in the switch: the supervisor runs without `--include-partial-messages` (supervisor 6.1, 8.3), as the old system does
today (A11).

---

## 14. Rollback

### 14.1 Matrix

Text of record: DESIGN 9.10 (with switch-final 23 C14.2 for the export's completeness).

| When | How to go back | What staff see |
|---|---|---|
| A1, A2 | 10.1 / 10.2 rollback; the switch module is inert until `move` is called (a revert of `08-` is another gated rollout) | nothing |
| A3, before the gate | `cutover rollback flip`, in order: (1) `import_control.rollback_at` set: pigeon routes creates through LU (born legacy) and keeps the T0→gate upload tee (composers hold old paths); (2) every pigeon-born conversation reverse-handed-over (14.2) once idle (no running turn, hold or background task, no open ask); (3) with zero owned: revert `20-old-connectors-0` (if pushed) and the switch commit (DNS, readers, monitor, HPA pin), `POST /__jarvis/admit` on every ui pod; (4) pigeon closes its sockets request-free and quiet (DESIGN 9.3) and they reconnect to the old path. Rehearsed in R3 | one reconnect per open tab |
| A3-A5, after releases | `cutover rollback conversation <id>…` or `--all`: per-conversation reverse hand-over (14.2) behind pigeon, which keeps fronting; the DNS stays on pigeon | at most one repaint of a reversed conversation |
| P4 | `cutover rollback old-up`: revert `40-` and `30-` (old backends and ui back to their A4 values; the bridge is still there), then per conversation as above, behind pigeon; the DNS stays on pigeon. A fault in pigeon itself is fixed forward or by rolling pigeon back one image. A DNS revert is only a stated exception Deyao decides on, in this order: reverse every owned conversation through magpie and the writer sidecar (no pigeon serving path needed; each once idle), wait for zero owned, then revert the switch commit and `20-` together (DESIGN 9.10) | at most one refresh |
| after P4 | no rollback to the old stack (it is deleted in A6); new-stack problems are fixed forward or rolled back to the previous image of the faulty component (expand-only schemas) | — |

Across the two clusters (which reverts go through Flux, which through `yyc-ai.yml`, and the bridge each step uses):
DESIGN 15.11.

### 14.2 Reverse hand-over of one conversation

For an owned, idle conversation (no active turn, no hold or background task, no open ask, no live sandbox or one
released by hermit's own idle rule); a conversation with an active new-stack turn is reversed after it ends and keeps
working, natively, meanwhile:
1. wait until the person's old child runs nothing (its running set empty, holds and background work included, no
   outstanding LU request) or is not running; the conversation stays owned and served natively while it waits;
2. reverse fence in magpie (native writes wait in their call for the export and the stop, seconds);
3. export (14.3), run by the import Deployment's writer sidecar (ready in advance, not a cold Job);
4. if the old child is running, `POST /__jarvis/restart?person=` on the pod holding it (DESIGN 9.6): it stops the
   child only if its running set is still empty, else answers `busy`, and then the conversation is unfenced back to
   owned (nothing changed) and retried later. The stop cuts nothing: with an empty running set no Claude Code process
   is up in the person's warm old sandboxes (background work keeps a session in the running set until the 30 min
   ceiling ends it, DESIGN 9.5);
5. `import_sources` reopened (or created for a magpie-born conversation), `released_at` null so the importer resumes
   syncing it, `migrating = true`, unfence into legacy;
6. pigeon routes the conversation to LU and sends one `agent.timeline.replacement` on every socket subscribed to it;
   the next LU request starts a fresh child, which reads the exported files.
A reversed conversation repaints once (step 6). The restart also gives each of the person's other shown legacy
conversations one repaint (a new epoch), the one refresh decision 11 allows on a rollback. No reload hook exists.
Rehearsed in R3, with the person's child holding the conversation cached.

### 14.3 Reverse export (`magpie import export --since <time>`)

A conversation with no event after its release needs nothing: its old files and sessions row are exactly what the
old system last wrote (`verify --all` proves the old side was not written after release, and the new side did not
write it either). For each conversation with events after `since` (or created after it), in the fenced state:
- **Transcript files**: reconstruct every cc file of the conversation and write it to
  `/jfs/users/<key>/claude/<cc path>` (write to a temp file in the same folder, then rename; uid/gid 1000, 0600 as
  Claude Code writes them). New conversations were created by the supervisor with `--session-id <conversation id>`,
  so their file is `projects/-workspace/<conversation id>.jsonl`, the name CloudCLI's disk discovery keys on.
- **Auto-memory** (decision 6): the conversation's `projects/-workspace/memory/**` goes to
  `/jfs/users/<key>/claude/aviary-memory/<conversation id>/`, a folder the old stack does not read, so nothing is
  merged into the person's shared old folder and nothing is lost; the importer ignores that folder.
- **Sessions rows** (`cloudcli`, the person's `cc_u<key>` schema): upsert `session_id`, `provider_session_id`,
  `custom_name`, `name_source`, `"isArchived"`, `deleted_at`, `updated_at`, `jsonl_path`; titles generated after the
  last export included. Deletions for good on the new side delete the row and the files. A person with no schema yet
  (first signed in after T0): no rows are written; CloudCLI creates the schema when their process first starts and its
  disk discovery indexes the exported transcripts.
- **Failed turns**: magpie's failed-turn rows after `since` → `jarvis_failed_turns` rows of the schema.
- **Shares made on the new side**: a `cc_shared.shares` row with the same id plus the frozen transcript under
  `/jfs/users/<key>/shares/<id>/` and the workspace clone back to `…/shares/<id>/ws` (metadata-only `juicefs clone`).
- **Sent files**: each `sent_files` row after `since` → `/jfs/sent/<conv>/<tool_use_id>/<rel>` from its blob.
- Uploads are already in the workspace (`ws/<conv>/uploads`, written by hermit), which both stacks share.
- Not exported: view-as rows (they stay in magpie's audit).

The export is the only importer command that writes to the old side: it runs in the import Deployment's writer
sidecar (`magpie import export-server`, idle until called, from `start-handover` until P4 ends), which mounts PVC
`hermit-jfs` read-write and connects to `cloudcli` with `MAGPIE_IMPORT_CLOUDCLI_URL` without the read-only session
option (the only process that omits it, 7.2). Rehearsed in A2 (R3).

---

## 15. What gets deleted

### 15.1 Cluster and cloud (P5 and P6, 10.5-10.6)

| Object | Where | Step |
|---|---|---|
| Namespace `jarvis` with Deployments `ui`, `web`, `egress`, `tools`, `cloudflared`; Services `ui`, `ui-peers`, `web`, `egress`, `tools`; PDBs; NetworkPolicies `ui-from-bridge`, `default-deny-ingress`, `egress-dns-namespace-internet`, `egress-from-sandbox-hosts`, `egress-ingress`, `egress-postgres`, `tools-ingress`, `tools-to-news-capture`, `tools-to-yyc-mcp`, `ui-ingress`, `ui-to-sandbox-hosts`, `web-from-news-monitor`, `web-from-ui`, `web-to-apiserver`, `web-to-sandbox-hosts`; ConfigMap `jarvis-config-*`; PVC `jfs`; Secrets and SealedSecrets `egress-env`, `ghcr-pull`, `sandbox-vm`, `tools-env`, `tunnel`, `ui-env`, `ui-juicefs`, `web-env`; LimitRange; RoleBindings; Role `magpie-import-legacy` and its RoleBinding | deploy prune (trailers), then the namespace by `yyc-apps` | A6 (10.5 step 4) |
| PV `yyc-ai-jfs` (Retain; the JuiceFS volume itself stays) | cluster | A6 |
| DaemonSet `kube-system/sbx-overlay-route`, RuntimeClass `gvisor` | cluster | A6 |
| Deployment `magpie-import`, the `yyc-ai` egress rules to `ui.jarvis` | yyc-ai | A6 (`import-done`, after P4) |
| ConfigMap `yyc/yyc-catalogue`: not deleted and not changed in A6 (its source moved to `catalogue.json` under the same key in the switch commit, D16) | yyc | A3 |
| `yyc/monitor-keys` key `JARVIS_OPS_TOKEN` (replaced by `HERMIT_OPS_TOKEN`) | yyc | A3 (switch commit) |
| NetworkPolicy peers in `yyc` admitting `jarvis` tools to yyc-mcp and the capture route | yyc | A6 |
| `pigeon-test` record and Access destinations | Cloudflare (tofu) | A3 (switch commit) |
| `ws`, `__jarvis` paths; tunnel `yyc-ai`, its config and token; custom hostname and Access destinations of `ai-yyc-ui.deyaochen.com` | Cloudflare (tofu) | A6 |
| Hand-set CNAME `ai-yyc-ui.deyaochen.com` in Deyao's zone | Cloudflare (Deyao's account) | A6 |
| Databases `jarvis`, `cloudcli`; roles `jarvis`, `cloudcli`; Sealed Secrets `pg-role-jarvis`, `pg-role-cloudcli` | CNPG | A6 (after the archive, 15.4) |
| Bucket prefix `yyc-ai-files/jarvis/files/` (the old chat UI's files) | bucket | A6, after its archive copy |
| Old fabric: EC2 instances, template, AMIs, snapshots, security group, IAM users; gateway leftovers on `yyc-pool-apps-worker2`; Hetzner route and firewall | AWS, Hetzner, node | A6 |
| JuiceFS `users/`, `sent/`, orphan `ws/` folders | JuiceFS | P6 |
| GHCR package `yyc-ai-ui`, later `jarvis3-sandbox` | GitHub | A6 |
| Store `yyc` keys of the old stack and of the window (list in 10.5 checklist) | store | A6 |

### 15.2 Code

| Repo | Delete | Keep |
|---|---|---|
| DE0CH/claudecodeui | `jarvis/router.mjs` (with its switch module), `jarvis/Dockerfile`, `jarvis/test/router.test.mjs`, `jarvis/test/fake-cloudcli.mjs`, `.github/workflows/jarvis-image.yml`; the sandbox form of Jarvis mode: `server/modules/jarvis/{sandbox-process,jarvis-guard (sandbox branch),jarvis-view-as,jarvis-shares,jarvis-shares.routes,jarvis-sent-files,jarvis-clone,jarvis-warm.routes,jarvis-purge (sandbox branch),jarvis-files.routes (sandbox branch),jarvis-title (sandbox branch),jarvis-history (legacy reset-note shim),jarvis-errors (sandbox codes)}.ts`, the `JARVIS_WEB_URL`/`JARVIS_CC_*`/`JARVIS_JFS_ROOT` paths and their tests (`jarvis-mode.test.ts`, `server/modules/jarvis/e2e`); `docs/paseo-ui.md` server parts; the `yyc-ai-ui` image | `paseo/` (the apps), `yyc-ios.yml`, `pigeon-app.yml`, `paseo-ui.yml` (path filter loses `server/modules/paseo/**` if that module goes); **everything Jarvis 4's local mode uses** (`jarvis/runtime-bundle.sh`, `jarvis4-runtime.yml`, `jarvis-local.routes.ts`, `jarvis-config.ts`'s local part, `server/modules/paseo/**` if local mode serves the Paseo UI): Jarvis 4 dogfooding is open, so the deletion PR must keep `jarvis4-runtime.yml`'s local-mode tests and bundle build green and must not touch local-mode code |
| DE0CH/jarvis3 | `chat/src/web/**`, `chat/sandbox/**`, `chat/sandboxd/**` (moved to aviary), `deploy/chat/**` (YYC writes egress's, tools' and cloudflared's manifests itself, platform 9.7-9.8), web's parts of `chat/src/common/{config,schema,ops,tokens}.ts` and `kv`, `.github/workflows/sandboxd.yml`, the web/sandbox parts of `chat.yml`, docs `CLAUDE-CODE.md` (UI cut-over, `/internal/cc`), `SANDBOX-VM.md`, `SANDBOXD-MEMORY.md` (moved to aviary), `API.md`'s `/internal/cc` section, `CONFIG.md`'s web/sandbox/ui keys, `CHAT.md`'s web parts; the `import-jarvis` subcommands (after P5.1) | egress, tools, their docs and tests, the ZDR literals the guard greps (10.1.1) |
| DE0CH/aviary | `crates/magpie-import` and the `magpie import` subcommands, pigeon's LU, magpie's window tables (A6, 10.5 step 7), the LU contract rig, the switch personas and legacy-in-a-box, hermit's `import-jarvis` and `legacy-down.sh` (after 10.5 step 3; `legacy-cleanup` after P6); the legacy-path set at A7 (10.7) | everything else, including pigeon's request-free-first drain |
| DE0CH/YYC | `yyc/deploy/legacy/`, `yyc/deploy/config.json`, the top-level `system-prompt.md`, `yyc/deploy/jobs/`, `.github/workflows/yyc-ai-ami.yml` (replaced by `hermit-ami.yml`), the ZDR guard's old half (all A6, 10.5 step 4); `yyc-ai/cutover/` (runner, patches, `freeze-check`, `window.json`), `yyc/infra/gvisor-node.sh`, `infra/secrets.txt` rows of the old Secrets, README sections on ui, the per-person processes, `/jfs` in UI pods, the old-UI rollback, `UI_CUTOVER`-era notes (gap-1 6); the zero-downtime and runbook text rewritten for the new stack | — |
| claude-env | skill `yyc-ai` rewritten (names, runbooks, rollback, ops); its scripts that drive ui pods (`ui-pod-chat`, view-as checks in ui pods) replaced by the ported probes; `ios-app-record.sh`'s `pwa-shell-ios` trigger fixed; a lesson for this switch's pitfalls | — |

### 15.3 jarvis3's Kustomize base

The new stack does not use jarvis3's `deploy/chat` base: YYC writes egress's, tools' and cloudflared's manifests
itself (platform 9.7-9.8) and pins only the `jarvis3-chat` image tag. From WP-25 on, `deploy/chat/egress.yaml` on
jarvis3 `main` names the old egress's image itself (`jarvis3-chat:f91907d349e6`), and from WP-26 on
`deploy/chat/tools.yaml` names the same image for the old tools (jarvis3's `src/main.ts` refuses `JARVIS_ROLE=tools`
and `egress`; `chat/src/tools` and `chat/src/common` are unchanged between YYC's tag 52c9b8c and f91907d, so the pin
rolls nothing back). An old-stack bump of the base and the image tag therefore moves web only (still started by
`JARVIS_ROLE`) and leaves its egress and tools unchanged. The old stack keeps its remote base pinned at
`3f7499a9…` in `legacy/kustomization.yaml` until P5.4. jarvis3 deletes `deploy/chat/**` once nothing pins a commit
that needs it to exist on GitHub (pinned commits stay fetchable, so the deletion can happen any time; it is listed in
A6 with the rest).

### 15.4 The old chat UI's data (decision 5: archived only, nothing imported, then deleted)

`jarvis`'s legacy tables (users 3, conversations 37 with 20 deleted, messages 92, turns 53, events, claw_sessions,
files, shares 3, blobs, blob_grants; A13.F0) and `yyc-ai-files/jarvis/files/` (38 objects, 3.3 MB). Nothing of it
is imported into magpie. In A6 (10.5 step 2): `pg_dump --table` of exactly those tables to
`yyc-pg-backups/archive/jarvis-old-ui-<date>.dump`, the prefix copied to `yyc-pg-backups/archive/jarvis-old-ui-files-<date>/`
(both under Object Lock, outside the dumps' expiry rule, so they are kept), a test restore of the dump into a scratch
database, then the prefix deleted; the tables go with database `jarvis` in step 6.

---

## 16. Failure modes (switch-level)

Text of record for the window: DESIGN 9; the rows below are the ones the runner acts on.

| Failure | Detected by | Effect | Response |
|---|---|---|---|
| The flip's tofu run fails | `cutover flip` (workflow red) | the DNS is unchanged; the `yyc-ai` deploy and `yyc-apps` may already have applied the commit's catalogue and monitor parts, which are content-identical or read-only changes | fix, re-run `flip`; or revert the switch commit |
| yyc-mcp sees a different catalogue after the switch commit | the probe (a staff token on yyc-mcp) | none expected: the key and path are unchanged and the users/admins equal (the P0 CI step) | revert the switch commit (14.1) |
| The edge keeps sending some new connections to the old tunnel after the flip | HK-vantage probes not 20/20; `move` keeps finding non-LU sockets | before the first move: old-path sockets until moved; after it: upgrades get 503 before the 101 and HTTP is passed on to pigeon, so nothing breaks | wait; `move` repeats until the gate; `old-connectors-0` and the gate wait for 60 s of zero sockets and zero in-flight non-LU HTTP |
| Established sockets on tunnel `yyc-ai` close at the record change | rehearsal R0 (before A3) | every open tab would reconnect at T0 without the request-free rule, so an in-flight send could fail | A3 does not start; the ingress-rule variant is measured; if neither keeps sockets, Deyao decides |
| pigeon unhealthy after T0 | probes, pigeon readiness, edge 5xx | new loads fail | before the gate: rollback flip; after: fix forward (a pigeon rollout is an ordinary reconnect) |
| New connectors drop (all) | tunnel connections API, edge 1033 | outage | a Deployment with PDB; checked in preflight; pods restart in seconds |
| A ui pod's `/__jarvis/sockets` report is missing or stale | the importer, `watch` | the gate is not set (or no new fence while it stays stale); legacy conversations keep working through LU | wait; investigate the pod |
| The move closes a socket with a request in flight | `switch_inflight_closes_total` (also counts a request frame read after the close decision) | one failed request | `hold on` and Deyao is told; prevented by the router's re-check of the outstanding, 5 s quiet and not-typing conditions at the close |
| A socket stays busy, opaque or its person keeps typing, and it is never moved | `move` reports it after 10 min | that tab stays on the old path; the old connectors cannot go to 0; nothing is read-only | Deyao is told with the person; the runner retries; an opaque socket ends only when its tab closes or Deyao accepts one close of it |
| Work starts on an old pod during the A1 rollout | `ui-gate` (running, held, preparing or pending work on an old pod) | the gate pauses the rollout before that pod's SIGTERM; one already terminating may cut it (`a1_rollout_cut_total`) | resume after 10 min quiet; a non-zero count is reported to Deyao (DESIGN 14) |
| An exec into a ui pod keeps failing | status shows that schema's conversations `holdsUnanswered` | those conversations stay legacy, fully working through LU | expected path; A2 step 4 proves the exec path beforehand |
| A ui pod restarts in the window | `watch` alarm | its children stop: a running old turn on it would be cut | prevented by the freeze and no planned node work A1-A5; if it happens anyway it is counted (`old_turn_cut_total`), reported to Deyao and investigated before new fences resume |
| Importer leader dies between fence and release | lock moves | that conversation stays fenced ≤ 3 s | the fence has a 3 s lease; the other replica re-stats and releases or unfences |
| magpie primary switchover during a fence | `magpie_fence_seconds` | fence and release wait | synchronous standby; ticket calls and fenced writes wait ≤ 3 s, then retry; nothing is lost |
| Verify mismatch at pre-verification | status `stuck` | that conversation stays legacy, fully working | 7.7; it blocks only A5 |
| A `pg` restart or JuiceFS metadata stall | importer stat timeouts | hand-overs pause (safe) | wait; the importer never releases on missing stats |
| Budget exhausted on one egress | 402 in that stack | that stack's turns fail with the budget sentence | the new egress counts the old stack's spend within a minute (8.3); preflight requires USD 10 headroom; budget edits are frozen A3-A5, so a raise needs Deyao's OK and goes to both |
| AWS vCPU quota hit by both fabrics | hermit `launch-failed` → `sandbox.busy` | a new-stack turn waits for capacity | preflight checks headroom for both fabrics' peak; the runner alarms on `preparing` p99 |
| Someone else pushes to YYC during the window | the runner's rebase, `freeze-check` | none | the runner rebases and re-checks; `freeze-check` refuses window-breaking changes |
| The Jarvis 4 session running the runner pauses or dies | state.json | `watch` stops; the hand-over itself continues inside the cluster | every step is idempotent: a new session re-runs the same subcommand |
| A bridge target fails (a node reboots, a callee pod moves) | `bridges.sh check` in `watch`, HAProxy's health check | none: traffic goes to the other targets within 11 s (a connect to a failed target is retried on another at once) | none (rehearsed in R6) |
| The bridge server yyc-bridge1 fails (one server, both bridges) | LU errors (`pigeon_lu_errors_total`), `bridges.sh check` | legacy conversations' requests through LU fail (B1), the pass-on fails (B2), B4-B6 fail | `hold on` (no new fences), Deyao told; `bridges.sh down all && bridges.sh up` rebuilds it; legacy conversations stay legacy and recover when it does; nothing already released is affected |
| The old cluster's API (single master) unreachable | the importer's B7 reads, `ui-gate`, `move` | releases wait (fail closed); LU and the ui pods keep serving | wait; N10 keeps `yyc-master1` untouched until A6 |
| The new cluster's API unreachable | `yyc-ai.yml` red, the runner's reads | no deploy; every new-stack Pod keeps serving | wait for the builder's runbook; the runner's waits resume |
| The new cluster lost as a whole (stated gap, DESIGN 15.11) | every probe of `ai.yyc-internal.com` through tunnel `pigeon`, the runner's reads | before the gate: none beyond the minutes to `rollback flip`; after releases: owned conversations down, legacy ones served by the old stack through tunnel `yyc-ai` if it still has connectors | before the gate `rollback flip` (old cluster and Cloudflare only); after releases: `hold on`, Deyao told; the builder restores the cluster (etcd snapshot from Object Storage, or a fresh build from the machine configs in git); `magpie-pg` recovered from `pg-pitr` (CNPG `bootstrap.recovery`, server name `magpie-pg`, base backup plus WAL) by the database folder's workflow; a `yyc-ai.yml` run; `bridges.sh check`; then the runner's `watch` resumes |
| Talos node work during A3-A5 | `window.json`'s hold, the upgrade workflow's skip | none | N8; a node that reboots anyway is a pod restart covered by the zero-downtime rules (or, for the gateway node, the stated gap) |

---

## 17. Changes the component specs need

Items already applied in a component spec at the time of this revision are marked "(done)" so the owners can skip
them. Since the switch-final revision, every item below about the 409 retry and its sentences, `legacy.cut`, the
synthetic import turn, `final`, `freeze` and the switch night's 35-minute overlap is superseded by DESIGN 9 (with the
mechanisms of `switch-final.md` it cites)
(DESIGN 12 D14); the component specs follow DESIGN 12, not these items, where they differ.

### 17.1 magpie (`spec-magpie.md`)

1. 19: the subcommand list is `magpie import plan|run|catch-up|verify|handover|holds|allow-handover|hold|release
   --force|reset|freeze|parity|export|status` (7.9 here), not only `plan|run|verify|catch-up`; where it runs is 7.1
   here (Jobs in P2, Deployment `magpie-import` with 2 replicas and a leader lock in P3, export Job on a rollback).
2. 4: add `import_sources`, `import_shares`, `import_control` (7.3) as an expand migration; `migrating` default
   `false`, set `true` by the importer at creation.
3. 16.1 and 8.2: 409 `{"code": "conversation_migrating", "message", "retryAfterMs"}` on M5, M6 (all three), M7, M8,
   M11, M16 create and `DELETE /v1/shares/{shareId}` of a share of a migrating conversation (6.4); 16.2 lease 409
   (done).
4. 8.8 watchdog, 8.3 driver sweeper, 9.6 purge, title commands, blob GC refs: skip `migrating` conversations; a
   `turns` row with `origin = 'import'` is never driven or watched.
5. 8.9: code `legacy.cut` (done).
6. 6.1 / 19.1 / 21.4.2: auto-memory (done; `plan` fails on a file, the open question is section 20 item 2 here).
7. 7.2: `conversation.created` `origin.kind = "import"` and `turn.started` `data.origin = "import"` (6.6).
8. Timeline rows (magpie 7.3, M9, M12) are the read path pigeon consumes, so the importer writes rows through the
   same projection and the synthetic turn's `turn` changes carry `timelineSeq` (6.6).
9. 16.4: `GET /v1/ops/stats` gains `importOpen` and `importStuck` (counts of `import_sources` rows not released, and
   of those with `last_error`) for hermit's ops page while the import tables exist (17.3 item 4); dropped with them.

### 17.2 pigeon (`spec-pigeon.md`)

1. 20: the test host is `pigeon-test.yyc-internal.com` on tunnel `pigeon`, served by both tunnels' host lists from
   P1 and removed in the switch commit (9.1).
2. 7.10: `conversation_migrating` handling and its sentences, `legacy.cut` in 7.9 (done).
3. 5.13 `server: pigeon/<sha12>` (done); 17.5 tofu names (done); 18.1 clients file (done); 17.1 seed (done).
4. 7.10: the refusal also comes from `DELETE /v1/shares/{shareId}` (share revoke) of a migrating conversation's share
   (6.4); same retry and the "being moved" sentence.

### 17.3 hermit (`spec-hermit.md`)

1. 22.3 step 3: the CronJob is named `hermit-import` (platform 9.12's name; the subcommand stays `import-jarvis`),
   with Secrets `hermit` and `hermit-import` (8.1).
2. 22.3 steps 4-5 and 22.5: replaced by sections 10-11 and 14 here; the overlap is A3 to A5 (DESIGN 12 D11).
3. 21.4/21.5: the "switch commit" is this spec's `patches/10-flip.patch` (D15) (done in substance).
4. 18 (ops page): show the importer's open and stuck conversations as a finding while Deployment `magpie-import`
   exists (read from magpie's database through magpie's `GET /v1/ops/stats`, which gains `importOpen` and
   `importStuck` for the window); removed in P5.7.

### 17.4 supervisor (`spec-supervisor.md`)

Nothing for the switch beyond what supervisor 23 already says (done): a lease on a migrating conversation answers 409
`conversation_migrating` and the supervisor waits unready, retrying every 30 s. Its `--session-id <conversation_id>`
rule for new conversations is what makes the reverse export's file names right (14.3). It runs without
`--include-partial-messages` (supervisor 6.1, 8.3), so the switch depends on no live-frame path; see 18 item 1 for
the open disagreement on live frames.

### 17.5 egress and tools (jarvis3; specified in platform 17.2)

Own databases `egress` and `tools`; `import-jarvis` subcommands by argv (8.2); `person` on `egress_log` rows;
NetworkPolicies in `yyc-ai` admitting the hermit fabric (10.203.0.0/16 on 3128 and 8081, hermit 21.1). For the ZDR
guard (10.1.1): `chat/src/egress/modelbody.ts`, `chat/src/egress/proxy.ts` and `chat/test/egress.test.ts` keep their
paths and the literals the guard greps; `egress.json` keeps `routes.model` with `upstream` and `model: true`, and
`models`, `defaultModel`, `smallModel`, `titleModel` at the top level.

### 17.6 platform (`spec-platform.md`)

1. R15 and 9.12: the importer is `magpie import …` from the magpie image with the full list of 17.1 item 1; P2 runs
   Jobs (`magpie-import-plan`, `magpie-import`, `magpie-import-catch-up-<n>`), A3 to the end of P4 runs Deployment
   `magpie-import` (2 replicas, leader lock, container `magpie-import`, args `[import, handover]`), a rollback runs Job
   `magpie-import-export`; the hermit and egress deltas are CronJobs `hermit-import`, `egress-import` (every
   minute), tools is CronJob `tools-import` (P2 and P5 runs). All mount PVC `hermit-jfs` read-only except the export
   Job. Secrets as 12.2 already has them (`<service>-import` with the legacy URLs; the service's own Secret for its
   bindings); no `MAGPIE_IMPORT_UI_PEER` or RW URL. The "catch-up every 10 s until no ui Pod" Job of 18.2 goes.
2. 9.1: `config.json` and `system-prompt.md` stay at `yyc/deploy/` with the `jarvis-config` and `yyc-catalogue`
   generators until the switch commit (catalogue) and P5.4 (both files), not under `legacy/` (D17); the `yyc-catalogue`
   generator is `files: [config.json=catalogue.json]` (key `config.json`, D16), not `files: [catalogue.json]` as 9.2
   shows; `legacy/` also holds the importer's Role/RoleBinding in `jarvis` (7.2); `jobs/` holds the import CronJobs.
3. 13.3 or a new 13.6: the rewritten ZDR guard of 10.1.1 (inputs, pin resolution through the GitHub API, the two
   halves, the paths filter), since platform owns the YYC checks; and the P0 CI step that `catalogue.json`'s
   `users`/`admins` equal `config.json`'s while both exist.
4. 14.1: both tunnels carry every host of `staff_ai_hosts` from P1, not only the test host on the new tunnel (D2:
   otherwise the switch commit changes the new tunnel's ingress in the same apply as the DNS, and the edge can route
   to the new tunnel before its rule exists).
5. 18: superseded by this spec's sections 10-11 (this spec wins, as its header says); 18.3's "magpie's catch-up ends
   and clears every `migrating` flag" is replaced by the per-conversation handover (6.3).
6. 9.11 RBAC: ServiceAccount `yyc-ai/magpie-import` with Role `magpie-import-legacy` in `jarvis` (`pods`
   get/list/watch, `pods/exec` create), for the switch only.
7. A coverage section (the reviewer's 131 features with target homes egress, tools, ops/deploy, apps, "every
   service" or "drop") belongs in platform, which owns those homes; section 19 here records the ones whose
   disposition is a switch or deletion step, so platform's table can point to this spec for them.

---

## 18. Inconsistencies between the component specs noticed while writing this

Resolved since the first draft: the magpie REST shapes (one set, magpie 16.1 = pigeon 14.1 under `/v1/`, camelCase,
platform R3/R6; this spec now uses them), the read-path owner (magpie owns the timeline rows; M9 and M12 are magpie's
routes, pigeon maps rows only; magpie 21.1 decision 3, pigeon 23), and the Service port (80 everywhere, platform R2).

Still open, to be reconciled by their authors (not decided here, none of them blocks the switch):
1. **Live frames.** magpie 7.5 and 16.2 build `POST /v1/sandbox/turns/{turnId}/live`, `LiveState` on M9 and `GET
   /v1/conversations/{id}/live`; pigeon 8.7 and 22.1 item 5 say there are no live frames in this release and ask
   magpie to build none; the supervisor sends none until pigeon's real-reducer contract test exists (supervisor 8.3).
   The switch is unaffected either way (17.4).
2. **Turn origin enum.** magpie R30 proposes `person | agent | import`; pigeon 22.1 item 3 says `user | cli | import`.
   This spec only writes `import`.
3. hermit 22.2 says the old and new fabric overlap for "at most the old UI's 15-minute drain"; with DESIGN 9 it is
   A3 to A5, a few hours (DESIGN 12 D11).
4. Platform 18 (one catch-up Job, a global clear of `migrating`, the switch commit also adding a Job) differs from
   sections 6-11 here; this spec wins (17.6 item 5).

---

## 19. Coverage of the gap-report items and inventory features this spec owns

Gap-report items:

| Item | Where |
|---|---|
| gap-1 5.1 cut-over lever (tunnel ingress service) | replaced by the DNS flip between two tunnels (9) |
| gap-1 5.2 signed-in path list shrink, kept in two repos | the switch commit (9.1 P3) + pigeon's CI test (pigeon 17.5), equality after P5 |
| gap-1 3.1 offline switch does not end sessions | unchanged by the switch; hermit's "who may use" answers it after |
| gap-1 3.2 dead old-UI rollback runbook | deleted (15.2); this spec's rollback replaces it until `import-done` |
| gap-1 3.4 test host `ai-yyc-ui` | kept until P5, then removed with its CNAME (D14) |
| gap-1 5.6 public `/healthz` | gone with ui; pigeon probes on 9090 |
| gap-0 11 drains | the one old drain (A1's ui rollout) runs only while the old pods hold nothing (DESIGN 9.6); new drains per component specs; decision 11's reconnects only (2) |
| gap-2 2.24 deploy/rollback | P5 rewrites the runbook; rollback in 14 |
| gap-2 2.29 live dashboard | the monitor moves in the switch commit (D15, hermit 21.4) |
| gap-2 5.10 cleanup of kv leaks, hub row, gone hosts, legacy rows | dropped with `jarvis` (P5.6); hermit imports only what it keeps |
| gap-3 3 dead notification code and tables | not imported (7.11); code deleted (15.2) |

Inventory features (the ones among them with target home ops, egress, tools, apps or "drop" whose disposition is a
switch or deletion step; the rest of those homes are platform's to record, 17.6 item 7):

| Id | Feature | Disposition here |
|---|---|---|
| A7.F0 | Flux GitOps for YYC AI (Kustomization `yyc-ai`) | changed: the `yyc-ai.yml` workflow applies the folder (deploy-workflows); Kustomizations `yyc-ai`, `yyc-ai-secrets` deleted with `deletionPolicy: Orphan` in A0; the folder gains the new stack and `legacy/` in P1.2, loses `legacy/` in A6 |
| A7.F1 | Remote base pinned to a jarvis3 commit | kept for the old stack only (`legacy/kustomization.yaml`) until P5.4; the new stack pins only the `jarvis3-chat` tag (15.3) |
| A7.F2 | Instance ConfigMap `jarvis-config` | kept for the old stack until P5.4 (generator stays at the top level, D17), then dropped with `config.json` |
| A7.F3 | Access catalogue ConfigMap `yyc/yyc-catalogue` | kept: same object, same key `config.json`, source `catalogue.json` from the switch commit (D16, hermit 21.5) |
| A7.F44 | Cloudflare tofu workflow | kept; path filter to `catalogue.json` in the switch commit (hermit 21.5) |
| A7.F51, A19.F21, A20.F15 | ZDR guard (test + workflow) | rewritten in P1.2 for both stacks, old half removed in P5.4 (10.1.1) |
| A7.F52 | OpenRouter key guardrail script (`yyc/infra/openrouter.py`) | changed in P1.2: allows the union of both stacks' models; `egress.json` only from P5.4 |
| A7.F53 | gVisor node script and inert RuntimeClass | dropped: RuntimeClass pruned in P5.4, `infra/gvisor-node.sh` deleted in P5.8 |
| A7.F54 | Deploy-a-new-version runbook | rewritten in P5.8 (README, skill `yyc-ai`) for platform 13.2's flow |
| A8.F18 | Auto-memory dir `projects/-workspace/memory/` | per conversation (decision 6); empty today; a file that appears is copied into each of the person's conversations (7.4.2) |
| A12.F6, A12.F16-F18 | CloudCLI `projects`, notification endpoints, api keys / credentials, provider models | not imported (7.11); dropped with `cloudcli` (P5.6) |
| A13.F1, A13.F2, A13.F7, A13.F9, A13.F12 | retired `jarvis` tables and columns, stale kv keys, `budget` ops rows, the vm legacy-provider path | dropped with `jarvis` (P5.6) and jarvis3's `chat/src/web` deletion (P5.8); nothing imported |
| A13.F6 | Budget spend depends on the full `egress_log` history | copied into `egress.egress_log` and kept in step every minute until P5 (8.2, 8.3) |
| A13.F22, A13.F23 | Doc drift: reset note, `UI_CUTOVER` and edge-Worker rollback switches | the documents are deleted or rewritten in P5.8 (15.2) |
| A19.F5 | NetworkPolicy `web-from-news-monitor` | pruned with `jarvis` (P5.4); the monitor's new peer is `hermit-in`, added in the switch commit (hermit 21.4) |
| A19.F12 | CronJob `mcp-cleanup` | unchanged: its file `/etc/yyc-catalogue/config.json` never disappears (D16) |
| A19.F13 | yyc-mcp rollout | unchanged: no yyc-mcp Pod rolls at any step of the switch |
| A19.F14 | web_fetch own-capture contract | the capture route's ingress admits `yyc-ai` tools in P1.5; the `jarvis` peer goes in P5.4 |
| A19.F23 | `mcp-image.yml` smoke test mounts the catalogue | mount source `catalogue.json` in the switch commit, container path unchanged (hermit 21.5) |
| A19.F25 | news-stream dashboard and MCP contract tests | their snapshot fixtures follow in the switch commit (hermit 21.4) |
| A19.F26 | OpenRouter key usage shown by the monitor | kept: the new egress uses the same key (`YYC_AI_OPENROUTER_KEY`, platform 12.2), so the name prefix the monitor shows is unchanged |
| A20.F16 | iOS simulator smoke (Maestro) | run against the main host in the A3-A5 checklist |
| A15.F0-F9 | web chunk skew | D13 (every ui build in 720 h), 13.2 |
| A15.F11-F15 | iOS skew and hello | 13.1 |
| A15.F16 | epoch reset | none at the move (the same old child serves through LU, so cursors stay valid); one `agent.timeline.replacement` in magpie's epoch per subscribed socket at release (DESIGN 9.3); none after (magpie's stable epoch) |
| A16.F0 | ui drain cuts turns | no drain cuts anything: running old turns finish on the old stack, and ui is scaled down only with nothing connected (A5) |
| A13.F0-F3 | legacy old-UI data | archived only, then deleted (decision 5, 15.4) |
| A13.F21 | rollback to the old chat UI | deleted |
| A17.F20 | backups | A6 final dumps and the archive |
| A19.F15-F17 | databases, dumps, pg access | P1.1, A6; `magpie-pg` 3-instance synchronous (pg-ha 4); Cluster `pg` single-instance (decision 10, platform 11.5, DESIGN 7.3) |
| A19.F24 | Flux wiring | changed: the `yyc-ai.yml` workflow (deploy-workflows); P1.2 layout, A6 |

---

## 20. Open questions for Deyao

None. Deyao's decisions of 2026-11-02 settled the old chat UI's data (decision 5: archive dump only, 15.4),
auto-memory (decision 6: per conversation, 7.4.2), the shared `pg` (decision 10: single instance), the switch's
reconnects (decision 11) and rehearsal R5 (decision 12: skipped). The switch day needs no input from Deyao (it runs
whenever it is ready, by a Claude session with store `yyc`); he gets one message when A5 is done and one when A6 is done.

---

## 21. Review round 1: where each point landed

| # | Point (severity) | Landed |
|---|---|---|
| 1 | 131 features with egress/tools/ops/apps/drop homes have no recorded disposition (major) | Platform's to add as its own coverage section (17.6 item 7); section 19 here now lists, by id, every one of them whose disposition is a switch or deletion step |
| 2 | ZDR guard not rewritten; it would fail or be switched off (major) | 10.1.1: reads `egress.json` (`models[].id`, `defaultModel`, `smallModel`, `titleModel`; `routes.model` to openrouter.ai with `model: true`), resolves the `jarvis3-chat:<sha12>` pin through the GitHub API, greps the same literals; keeps an old half for `legacy/` while it exists, because both stacks call the model; lands in P1.2 (which moves the remote base the old guard parses), on the P0 and P1 checklists; old half removed in P5.4 with `config.json`; egress keeps the literals (17.5) |
| 3 | Catalogue key rename breaks yyc-mcp and `mcp-cleanup`; smoke test, docs, timing (major) | D16: key `config.json` kept, `files: [config.json=catalogue.json]`; every reader, the smoke test and the texts move in the one switch commit (D15, hermit 21.5's list); P5.4 changes no reader; `config.json` stays at its path until P5.4 (D17) so nothing breaks earlier |
| 4 | Importer packaging conflicts with R15 (major) | D4, 7.1: `magpie import …` in the magpie image; P2 Jobs, P3 Deployment `magpie-import` (2 replicas, leader lock), export Job; CronJobs `hermit-import`, `egress-import`, `tools-import`; PVC `hermit-jfs` read-only; Secrets per platform 12.2; platform R15/9.12 edits in 17.6 item 1 |
| 5 | tofu names disagree (major) | 9.1 uses platform 14.1's: tunnel `pigeon`, output `pigeon_tunnel_token`, store key `YYC_CF_PIGEON_TUNNEL_TOKEN`, one list `staff_ai_paths`, service `pigeon.yyc-ai.svc`; pigeon 17.5 already references them; the path shrink moves to the switch commit as platform 14.2 has it |
| 6 | Migration refusal shape breaks R6; pigeon lacks it (major) | 6.4 and D8: 409 `{code: "conversation_migrating", message, retryAfterMs}`; 6.5 points to pigeon 7.10 (retry ≤ 45 s, both sentences, `legacy.cut` in 7.9); every magpie call here is in magpie 16.1 / pigeon 14.1's `/v1` camelCase shapes; 18's "uses magpie's shapes" removed |
| 7 | Planned unavailability; per-person release holds idle conversations (minor) | D3, 6.3: release per conversation, from each CloudCLI child's running list by session id (read inside the ui pod, no old-stack change), plus stable sources and sessions row over two looks and a released old sandbox; the earlier accepted gaps are superseded: the switch has none (decision 4, switch-final) |
| 8 | Two contracts for pigeon's read path (blocker) | Settled by magpie and pigeon: magpie owns the timeline rows (M9 long-poll with `waitMs`, M12 `/v1/changes`), pigeon maps rows only; this spec follows (12, 7.12 item 6, 18). The events-only alternative was rejected by both owners (magpie 24.1, pigeon 23.1) |
| 9 | Live streaming specified only on the supervisor side (major) | Not a switch matter: the supervisor ships without `--include-partial-messages` (supervisor 8.3), so neither imported nor new conversations use live frames at the switch (13.3, 17.4); the remaining magpie/pigeon disagreement is 18 item 1 |
| 10 | Importer over-built for 28 conversations (minor) | Taken in part: no mirror between P2 and P3 (one bulk import, optional catch-up Jobs, the final per-conversation sync at handover on the same code path, section 2, 7.4-7.5); the 5 s passes run only from `start-handover` (A3) until P4 ends. Not taken: dropping the reverse export (22 point 2) |
| 11 | Auto-memory silently dropped (minor) | Nothing to drop today (empty for all 10 people, A8.F18); `plan` fails naming any file that appears, and the behaviour change is section 20 item 2 |

---

## 22. Rejected review points

1. **Point 10's "drop the 5 s loop" for the switch window itself.** The hand-over loop runs from `start-handover`
   (A3) until P4 ends, and only then: its running mirror is what shows a legacy conversation's old turn as running in
   pigeon's lists while it finishes on the old stack (D9; without it the list would show it idle for as long as the
   turn and its hold last, up to 30 min after the turn), it pre-verifies so a release only re-stats, and the release
   conditions of 6.3 (running lists, two stable looks, an old sandbox released) have to be observed repeatedly in any
   case. Between P2 and A3 nothing loops, as the point asked.
2. **Point 10's "replace the reverse export with 'rollback = point the old stack back at its untouched JuiceFS
   files'".** Kept, because that rollback loses work: from its release on, a conversation takes new turns on the new
   stack only, with real staff traffic until `import-done`; pointing the old stack at the old files would
   drop every turn made since. The reviewer's case does hold for conversations with no new-side change, and 14.3 now
   says those need nothing. The export itself is small: it reads cc files through magpie's own reconstruct path (the
   FUSE read path, already built), writes them back under the old names, and upserts eight columns of the sessions
   row; it is rehearsed once in P2 (rehearsal 10).
3. **Point 2's "the guard lands in the same commit that deletes config.json".** The guard's new half lands earlier,
   in P1.2: that commit moves the remote base the old guard parses into `legacy/`, so the old guard would fail from
   P1 on, and the new egress already serves (test) model calls from P1, so it needs the check from then. The
   deletion commit (P5.4) removes the old half, which is what the point asked for in substance.
4. **Point 8's recommended design (magpie serves only events, pigeon projects).** Decided against by both owners
   (magpie 24 point 1, pigeon 23 point 1: the app's dense-seq gate needs a row counter only the event owner can keep
   at page-size cost). This spec takes their decision.
5. **Point 9's "add the live ingest, a live entry and `GET …/live` to magpie, and live frames to pigeon".** Out of
   this spec's scope and not needed by the switch (17.4); magpie and pigeon disagree on whether to build it in this
   release (18 item 1).

---

## 23. Two clusters (decision 13)

Text of record: DESIGN 15. What it changes in this spec beyond the edits listed in the revision note:

| Where | Change |
|---|---|
| 1.1-1.2 | the starting point is the old k3s cluster; the end state is the new stack on the new Talos cluster, with `pg` following in the Talos session's T5 after A6 |
| 5.2 | every "new" row runs on the new cluster, every "old" row on the old one; add a row "bridges B1-B7 (DESIGN 15.5): from A1; B1, B2, B7 gone at A6; B4-B6 at their apps' T4 moves; B3 until T5" |
| 7.1 | the `magpie-import` Deployment and the import Jobs run on the new cluster; the export Job writes `/jfs/users/…` through the new cluster's JuiceFS CSI |
| 7.10 | an old-API outage (B7) is "running-set read unanswered": fail closed, as the existing row for an exec failure |
| 8.1-8.2 | `hermit import-jarvis`, `egress import-jarvis` and `tools import-jarvis` run on the new cluster and read `jarvis` through `pg-shared` (B3) |
| 9.1 | unchanged tofu; tunnel `pigeon`'s connectors are on the new cluster, tunnel `yyc-ai`'s on the old; the ingress-rule fallback of 9.3 points tunnel `yyc-ai`'s rule at `http://pigeon-bridge.jarvis.svc.cluster.local:8080` (B2), since its connectors cannot reach a Service of the new cluster |
| 10.2 | rehearsal R6 (bridges, DESIGN 15.5) first; R1's personas run with the LU path through B1 |
| 10.3 | `retire-old`'s patches `30-` and `40-` are Flux commits on the old cluster |
| 10.5 (A6) | the teardown is two commits: the old cluster's (delete `yyc-ai/legacy/` except `legacy/catalogue/`; Flux prunes `jarvis` and the old cluster-scoped objects; then the `clusters/hetzner` commit for Namespace `jarvis`) and the new cluster's (`Deploy-Prune:` for the window objects); `bridges.sh down` for B1 and B2 (the server yyc-bridge1 with its last link); the old fabric's `legacy-down.sh` on `yyc-pool-apps-worker2`, after which that server is free for the Talos session's T2 |
| 10.6 (P6) | `hermit legacy-cleanup` runs on the new cluster (JuiceFS through its CSI) |
| 11.1 `flip` | waits for the `cloudflare.yml` run, the `yyc-ai.yml` run (catalogue readers and pigeon's HPA pin on the new cluster) and Flux on the old cluster (`yyc-ai/legacy/catalogue/` switching to `catalogue.json`, Kustomization `yyc-apps` for the monitor's hermit URL through B4) |
| 11.1 `move`, `allow-handover`, `rollback` | `move` and `admit` on every ui pod through the old cluster's admin path; the gate's ui-pod enumeration through B7 |
| 14.2 | the reverse hand-over's `restart` reaches the person's old child through B7's pod proxy; the export writes into the shared JuiceFS volume from the new cluster |
| 15.1 | add: the bridge server yyc-bridge1, NodePort and selector-less bridge Services, `YYC_OLD_CLUSTER_IMPORT_TOKEN` (A6; B4-B6 at T4); `infra/yyc-cluster-ca.pem` goes with the old cluster (T6) |
| 16 | the rows across clusters (above) |
| 20 | nothing to ask: the order against the Talos steps is DESIGN 15.2 N10-N11, and the bridges' cost is recorded in DESIGN 14 |


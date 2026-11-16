# Switch-over, final design: idle hand-over behind a bridging pigeon (A, with B's best parts)

**Superseded in part (2026-11-02, decisions 10-12).** DESIGN.md section 9 is now the switch's text of record. This
document still specifies the mechanisms DESIGN 9 keeps and cites by number (the legacy upstream's contract, tickets
closing on an outcome, the quiet period, fence and release with the re-stat of every fingerprint, upload ingestion and
legacy-path resolution, reverse-export completeness, `holds.js`). Its router observer and move rule, person states,
proxy sockets, seq remap and timeline capture, the in-place router upgrade, the 72 h tab question, the eager upload
tee, the egress export and the ≥ 7-day measurement are replaced by DESIGN 9 (list in DESIGN 9.11); where the two
differ, DESIGN 9 wins.

Status: the switch-over design of record, 2026-11-02 (before the revision above). It merges `switch-A.md` (winner) and `switch-B.md` against
`DECISIONS.md` (decision 4: no gaps; decision 9: working hours, no quiet-hours window; decision 1: GitHub workflows,
no Flux for yyc-ai). It replaces DESIGN.md section 9 and stated gap 7.3.3. Where this document says "as A §n", the
text of `switch-A.md` section n holds unchanged and is part of this design; everything else in `spec-cutover.md`
that neither document changes (the importer's sync code, byte-exact verification, the one-off copies, the tofu of the
front door, the deletion lists) stands.

Contents
1. Verdict
2. The design in one page
3. What changed from A (grafts from B, corrections, decision 9)
4. Pieces added for the window
5. States and invariants
6. The front door
7. Tabs and moves
8. The per-conversation hand-over
9. Running turns and asks
10. Attachments, sent files, files panel, shares, warm
11. View-as, lists, titles, creates, deletes
12. Retiring the old stack
13. Rollback
14. Failure modes
15. Verification: proving zero gaps
16. Phases and the switch day
17. What is deleted
18. Cost
19. Changes to DESIGN.md and the specs
20. Residual risks
21. Questions for Deyao
22. What was taken from B, and what was not
23. Attack findings and their fixes (binding; overrides earlier text on conflict)
24. Rejected findings

---

## 1. Verdict

### 1.1 Scores (5 = best)

| Criterion | A: idle hand-over behind a bridging pigeon | B: per-person fence at the old router, live adoption | Why |
|---|---|---|---|
| Truly zero user-visible gaps (decision 4) | **4** | 3 | A never closes a shown tab and never re-renders a shown conversation (seq remap, conversion only when not shown). Its one open point is a tab that is never hidden (policy question, 21 Q1). B has a *certain* visible effect: one in-place timeline replacement per open conversation at the fence (its own R4), plus held actions of up to 15 s (then a 45 s silent backstop), adopter failover pauses in a streaming reply, and one reconnect for unspliceable sockets |
| Risk | **4** | 2 | A changes no old-stack code path (the old router, CloudCLI, the old sandbox agent run as they are). B changes all three in production days before the switch (agent v2 in every old sandbox, router v2 terminating every WebSocket, CloudCLI stage tracking and handoff), adds a remote stream-json controller that must rebuild the Agent SDK's hidden state from logs, and puts a `legacy` sandbox kind into hermit, the component that grants leases. A's one real risk, the privileged packet-sniffing tab mover, is removed in this merge (3.1) |
| Build cost | **4** (≈ 25 days net after the merge) | 2 (≈ 40 days net) | B's adopter alone is 9 days and the old-stack bridges 11 |
| Rollback | **5** | 3 | A: the same fence in reverse, per conversation, behind the same pigeon, invisible. B: per-person fence-back with one reconnect per tab and a possible failed request (its R9) |
| Testability | **4** | 4 | Both run the real pinned old ui image in CI against the new stack. A's legacy-in-a-box nightly and property tests (ownership, remap) are strong; its pcap-fixture tests were its weakest part and go with the tab mover. B's log-replay property test is strong, but the behaviour of its modified old code in production is only proven by a 3-day soak |
| Fit with the clean-cut rule | 3 | 3 | Both keep one writer per conversation at every instant and delete every bridge in the same plan. A's window can be long (hours to days) and has two temporary dual writes (upload tee, egress log copy). B's cut is per person and short, but needs 9 temporary components in 4 repos, including changes to code that is about to be deleted |
| Fit with decision 9 (working hours) | **5** | 3 | A gets easier in working hours (tabs hide often, turns finish on their own). B's fence in working hours meets many running turns, asks and uploads in flight: every one becomes a live adoption or a held request |

**A wins** on four of six criteria and ties on the other two. The decisive points: A's zero-gap claim rests on
mechanisms that leave the old stack untouched (a running turn simply finishes where it started), while B's rests on
live-adopting a running Agent SDK session from the outside, which is the most fragile option available and buys
nothing once the old stack can stay up behind pigeon.

### 1.2 Facts checked for this verdict (read-only, 2026-11-02)

- The old router already knows every client socket per person (`router.mjs` 459, 639, 1200-1210: `p.sockets`,
  `forwarded` with the person key) and sees every byte of it (`socket.on("data")`); it exposes none of this today.
- Old WebSockets are uncompressed (`websocket-server.service.ts` 84: no `perMessageDeflate`, `ws` 8 default off), so
  frames can be read from the piped bytes.
- The app sends `client_heartbeat {appVisible}` every 15 s (`client-activity-tracker.ts` 1, `use-client-activity.ts`
  52-77: `document.visibilityState` on the web, `AppState` on iOS).
- The app shows its "正在重新連線…" toast at once on any disconnect (`timeline/sync-status.tsx` 23-38: no delay), so a
  close of a shown tab is always visible. Moves must happen only while hidden: both designs' premise holds.
- **Asks are not answerable on the old stack at all.** The Paseo hub drops `permission_request`
  (`paseo-hub.ts` 335-337), every agent snapshot says `pendingPermissions: []` (`paseo-connection.ts` 609), and
  `agent_permission_response` gets `rpc_error "Not available here"` (inventory area 11, feature 11 and surprise 0).
  An old turn that calls AskUserQuestion hangs until Stop. A §8.3 ("LU relays the permission request and the answer")
  and B §9 ("the prompt the tab shows is the same object") both describe behaviour that does not exist. Corrected in
  9.3.
- Access session duration of the staff app is 720 h (`news-stream/deploy/cloudflare/tofu/access.tf` 69).
- ui deploys are routine: about 50 commits touching yyc-ai ui pins since 2026-10-01, the latest today.
- At 15:41 HKT today the two ui pods held 2 and 3 established TCP connections on :8080 (including peer and probe
  connections). Old sockets exist in working hours; the design must move them, not assume there are none.

---

## 2. The design in one page

1. **pigeon fronts both stacks for the window** (as A §1 point 1). Its temporary *legacy upstream* (LU) speaks the
   Paseo protocol and the small REST surface to the old `ui` router as the person, with the tab's own Access token,
   for every conversation the old stack still owns. A legacy conversation looks and behaves in a pigeon tab exactly
   as in an old tab: same old child, same epoch and seqs, same live stream, Stop, uploads.
2. **At T0 only the DNS moves.** Old connectors, ui pods and CloudCLI children stay up. Established sockets stay on
   the old path; new connections land on pigeon. Nothing in namespace `jarvis` changes at T0.
3. **Old sockets leave only while nobody looks**: naturally (tab closed, reload, sleep, network change, iOS
   background) or, while the tab's own heartbeat says hidden and it has no request in flight, by a close the
   **old router itself** performs on the importer's command (the *router observer*, 4.2; it replaces A's
   privileged tab mover). The tab reconnects while hidden, lands on pigeon, and its cursors are still valid.
4. **A conversation is handed over only when it is idle and reachable only through pigeon**: no old turn, no hold,
   no warm old sandbox, and its person has no old-path socket left. A sub-second fence in magpie, a re-stat against
   the pre-verified copy, a release transaction; pigeon switches that conversation from LU to magpie on every socket
   and keeps the tab's seq space (remap) until the tab is not showing it.
5. **Running turns never cross.** They finish on the old stack, live in every tab through LU, with Stop working. No
   process migration, no `legacy.cut`, no deadline.
6. **The old stack retires last**, when every conversation is owned by magpie and no socket needs LU. Rollback is the
   same hand-over in reverse, per conversation, behind the same pigeon.
7. **The switch runs in working hours** (decision 9), with Claude watching the runner's live state; there is no
   04:00 window anywhere in the plan.

---

## 3. What changed from A

### 3.1 Grafts from B

| # | From B | Into the final design | Replaces or fixes in A |
|---|---|---|---|
| F1 | B's core insight: the old router is the one place every old request passes, and it already routes per person (B §1, 3.3) | **Router observer** (4.2): a small, observe-only addition to `router.mjs` that reads client heartbeats and request ids from the bytes it already pipes, reports sockets per person, and closes one socket on command. It does not terminate, buffer, rewrite or re-route anything (none of B's router v2) | A's tab mover (A §3.4): a hostPID, NET_ADMIN, SYS_ADMIN DaemonSet with AF_PACKET capture, TCP reassembly, mid-stream WebSocket joins, guessed person attribution and SOCK_DESTROY. Removes A's risks 3 and 4 (an unattributed connection blocking everyone's hand-over; privileged code on production nodes) |
| F2 | B §7.1: a server-side close rejects every request the client has in flight (`daemon-client.ts`: `DAEMON_CONNECTION_LOST`), so a close can fail a send even on a tab that is about to hide | Every move (router observer and pigeon proxy sockets) also requires **zero outstanding request ids** on that socket and no inbound message other than a heartbeat in the last 5 s (7.3) | A's move rule looked only at visibility |
| F3 | B §8: ingest every file in each person's `cloudcli-assets/` as a magpie upload and resolve old paths at send time (`import_uploads`) | **Legacy upload resolution** (10.1): a file attached *before* T0 (old child path in the composer) and sent after its conversation was released resolves to the same file in magpie | A's tee only covered uploads made after T0; a pre-T0 attachment sent natively after release would have failed with `unknown_upload` (a G2-type gap A did not close) |
| F4 | B §14.1: zero-gap counters that must read 0 for the whole window | Merged with A's gap recorder into one pass/fail set (15.1), alerted live | A had the recorder but no production counter set |
| F5 | B SW6, SW13-SW16: sends at random instants across the cut, crashes in the middle of the hand-over | Fence-race fuzz (1 000 sends with jitter around a fence), importer leader kill between fence and release, pigeon replica kill with LU connections, magpie primary switchover during a fence (15.3) | A had one fence-race persona |
| F6 | B §12.2: hermit warm hints keep a host up through the window | Warm hints during A4 so a released conversation's first new-stack turn never waits for a cold host (8.4) | A accepted "preparing as for any first turn after a pause" |
| F7 | B §14.4: one rehearsal identity holding every state at once (long turn, hold, ask, idle warm sandbox, unsent attachment, web + iOS, view-as) | Rehearsal R1 (15.5) | A rehearsed the states one at a time |
| F8 | B §3.3: durable switch state, never default to the old path or a fresh child on missing state | pigeon and the importer treat a missing or stale observer report as "the person may have an old-path socket" (5.4); the gate and per-person state live in magpie, never in memory | A had this for presence; now explicit for every input |

### 3.2 Corrections to A

| # | A said | Holds |
|---|---|---|
| C1 | asks are relayed and answerable through LU; the child replays pending permissions (A §8.3, 15.2 "ask" persona) | the old stack cannot answer asks at all (1.2). An old turn's ask stays as it is today: the turn waits and Stop ends it, from any tab, through LU. Nothing is lost or changed. After release, magpie's native ask flow applies (better than before, not a gap). The "ask" persona asserts Stop parity instead (15.2) |
| C2 | A3 flip at 04:00 HKT; "expected A4 length" from night-time numbers | decision 9: T0 on a weekday in working hours (16.2). The window length comes from the observer's measurement in A2 (15.4) |
| C3 | ui frozen from A2; claudecodeui changes "wait or ship to pigeon-app only after A6" | ui *and* `pigeon-app` stay at one claudecodeui commit from the observer release (A1) to A5: S1 persons' pigeon-served pages talk to the old child, so the app and the child must match (16.1) |
| C4 | LU reuses the token of the tab's upgrade | pigeon keeps, per person, the newest Access token it has verified (any request of that person refreshes it) and opens or reopens LU with it. Expiry then behaves exactly as without the switch: when the person's Access session ends, both stacks ask for sign-in |
| C5 | the LU marker `x-aviary-lu` is a plain header the old router "passes on" | a header any client could send must not make an old-path socket look like LU (that would let a conversation be fenced while an old-path socket can still write it). The router observer counts a socket as LU only when `x-aviary-lu` carries pigeon's pair token (`YYC_TOKEN_PIGEON_UI`), and strips the header before the child sees it. Anything else is old-path |
| C6 | "applied by git like everything else" with Flux implied by DESIGN 11 | every step is a commit to DE0CH/YYC `main` applied by the yyc-ai GitHub workflow (decision 1); the runner waits on the workflow run, not on a Flux reconcile |

---

## 4. Pieces added for the window

All migration-only, deleted in A6 (section 17). None is a deployment switch: LU is reachable only for a conversation
that magpie's row says is legacy-owned, which only the importer can make true.

### 4.1 pigeon `crates/pigeon/src/legacy/`

As A §3.1 (LU connection and LU HTTP, proxy sockets, native sockets with legacy routing, seq remap, presence report,
upload tee, legacy view-as key), with C4, C5 and F2:
- the LU upgrade carries `x-aviary-lu: <YYC_TOKEN_PIGEON_UI>` plus `<pod>/<socket id>`;
- pigeon tracks outstanding request ids on proxy sockets (it already parses their frames) for the move rule (7.3);
- pigeon's drain closes hidden, request-free sockets first (kept after A6; it improves every rollout).

### 4.2 Router observer (`jarvis/router.mjs`, claudecodeui; replaces A's tab mover)

**The A1 release is not an ordinary rolling deploy.** A normal ui rollout stops every CloudCLI child after the
drain, each child comes back with a fresh epoch (`paseo-timeline.ts`: new epoch on every process start), and the old
sandbox agent kills Claude Code's process group when `/proc` drops — so every open tab reconnects and every shown
conversation's timeline is replaced. That is a switch gap (decision 4), not "as every ui deploy did". The observer
therefore ships in a **router that upgrades in place** (23 C1): the new `router.mjs` process inherits the listening
socket and every child `/proc` fd over `SCM_RIGHTS` from the old process, so no child is restarted, no epoch changes
and no socket closes. The A1 release also bundles **every** old-side change the window needs, so the window needs no
second ui rollout: the observer, raised `JARVIS_UI_MAX_USERS` and pod memory (23 C2), `purgeWake`/purge off (23 C9),
LU-aware eviction (23 C2), a hot-reloadable staff allowlist / admins / budget / model list (23 C7, C11), and the
in-place-upgrade entrypoint itself. From A1 to A5 ui and `pigeon-app` stay at that one claudecodeui commit, and the
runner forbids any further ui rollout (23 C1).

| Part | Behaviour |
|---|---|
| socket record | at each first-hop upgrade (`#onUpgrade`, both the local `p.sockets` and the `forwarded` branch): `{id, person, viewAs, kind, openedAt}`. `kind = lu` only with a valid pigeon pair token in `x-aviary-lu` (constant-time compare), else `old`. The header is removed in `upstreamHeaders` |
| client tap | a listener on the existing `socket.on("data")` feeds a frame reader (FIN/opcode/mask/length, masked client frames, continuation frames) that keeps for each text message only its `type`, `requestId` and, for `client_heartbeat`, `appVisible` and the time. It never touches the pipe: the reader runs after the bytes are already written to `usock`, inside try/catch |
| server tap | the same on `usock.on("data")` (unmasked server frames): removes a request id when an outbound message's `payload.requestId` (or an `rpc_error` for it) matches |
| opaque sockets | a socket whose 101 negotiated an extension, or whose reader hit anything it cannot parse, becomes `opaque`: counted as old-path, never moved, alerted (expected 0). Conservative: it only delays that person's hand-over |
| `GET /__jarvis/sockets` | peer-style auth with the importer's pair token (`YYC_TOKEN_MAGPIE_IMPORT_UI`); answers this replica's first-hop sockets: `{id, person, kind, viewAs, opaque, lastHeartbeat: {appVisible, at}, hiddenSince, outstanding, lastInboundAt}` and `generatedAt` |
| `POST /__jarvis/sockets/{id}/close` | same auth; re-checks the move rule locally (7.3) against its own live state at that instant and, if it still holds, destroys exactly that client socket (the existing `close` handlers clean up `p.sockets`/`forwarded` and the child side). Answers `{closed}` or `{refused, why}` |
| size | about 300 lines plus cases in `jarvis/test/router.test.mjs` (fake CloudCLI): bytes through the router are identical with and without the observer; LU tokens; opaque; close refused when a request is outstanding; drain unchanged |

### 4.3 magpie: legacy routes and tables

As A §3.2 (tickets, fence, fenced wait, anchors, presence, `legacy_fence_at`, `uploads.legacy_path`), plus F3's
`import_uploads (legacy_path pk, person, upload_id, sha256, imported_at)` and resolution in M3/M7: a `files[].path`
under the old assets prefix (`/var/lib/cloudcli/home/u…/.claude/cloudcli-assets/`) resolves through `uploads.legacy_path`
(post-T0 tee) or `import_uploads` (pre-T0) to that person's upload; unknown → `400 unknown_upload` (alert, expected 0).
Presence sources become `pigeon:<pod>` and `router:<ui pod>` (written by the importer from the observer reports).

### 4.4 The importer (`magpie import handover`)

As A §3.3 (no cut, no deadline, pre-verification, wake on dirty, fence protocol, reverse hand-over, no synthetic
import turn), plus:
- the **mover loop** (formerly the tab mover's logic): every 5 s it reads `/__jarvis/sockets` from every ui pod
  (headless `ui-peers`), writes `router:<pod>` presence (TTL 15 s), and asks the router to close sockets that meet
  the move rule once the runner has armed moves;
- **upload ingestion** (F3): from A2 on, every person's `users/<key>/claude/cloudcli-assets/` into `import_uploads`
  (bytes to the bucket), the whole folder on the first pass, new files every 5 s, and one more pass of a person's
  folder inside each of their conversations' release transactions;
- **warm hints** (F6) to hermit every 60 s while any conversation is legacy-owned.

### 4.5 Small pieces

- NetworkPolicies: `jarvis/ui-from-yyc-ai` (ingress to `app=ui` :8080 from `yyc-ai` `app=pigeon` and
  `app=magpie-import`); egress rules for those two in `yyc-ai`.
- Store `yyc`: `YYC_TOKEN_PIGEON_UI`, `YYC_TOKEN_MAGPIE_IMPORT_UI`, `YYC_PIGEON_LEGACY_VIEW_AS_KEY` (as A §3.1).
- `egress-export-jarvis` CronJob (as A §3.5): each egress sees the other's spend within a minute.
- The cutover runner gains the steps of 16.

---

## 5. States and invariants

### 5.1 Per conversation, per person

As A §4.1 and §4.2 (`[legacy] → [fenced] → [owned]`; persons S1 bridged → S2 native → S3 handing over → S4 done),
with "old-path socket" now read from the router observer: p is S1 while any `router:*` report lists an `old` or
`opaque` socket of p, or while any ui pod's report is missing or older than 15 s.

### 5.2 Invariants (each a test, 15.3)

As A §4.3, items 1-5, plus:
6. No socket is closed by the switch while it has an outstanding request id or while its last heartbeat said
   `appVisible: true` (F2).
7. Every file path a tab holds in its composer at any instant resolves on the path that receives the send (F3).

### 5.3 Ownership in one sentence

A conversation has exactly one writer at every instant: the old stack (reached only through an old-path socket or
through LU with a ticket) or magpie; the fence commits only with no open ticket and no old-path socket of the person,
and the release only after a clean re-stat.

### 5.4 Conservative inputs

Every input that gates a hand-over fails closed: a stale or missing observer report, an opaque socket, an unknown
ticket state, an unreachable ui pod or a running-set read that failed all mean "not yet". None of them changes what a
tab shows; they only lengthen A4.

---

## 6. The front door

As A §5, with C6: the switch commit (DNS of `origin_ai` to tunnel `pigeon`, catalogue readers on `catalogue.json`,
the monitor on hermit's snapshot) is pushed to YYC `main` and applied by its workflow; `ws` and `__jarvis` stay on
both tunnels' signed-in path lists until A5; nothing in `jarvis` is scaled. The runner checks the flip is live (20
consecutive `server: pigeon/` answers), then arms moves and sets the gate.

**Gate before A3: established sockets survive the DNS change.** A rests on Cloudflare keeping an established
WebSocket on tunnel `yyc-ai` after the record points elsewhere. Rehearsal R0 (15.5) proves it on the test host with
the real tunnels: 20 sockets held open across the flip for 1 h, zero closes. If R0 fails, the flip is done instead by
pointing tunnel `yyc-ai`'s ingress rule for the host at `http://pigeon.yyc-ai.svc` (cloudflared applies a new remote
configuration to new requests only), which R0 then measures the same way; if neither keeps sockets, A3 does not start
and Deyao is asked (a T0 that closes open tabs is a gap).

---

## 7. Tabs and moves

### 7.1 Socket kinds and routing

As A §6.1-6.3 (old-path, proxy, native; S1 → proxy sockets, S2+ → native with per-message legacy routing; the
routing table of A §6.3).

### 7.2 Seq continuity

As A §6.4-6.5: a moved tab's cursors stay valid (same old child); at release pigeon keeps the tab's epoch `E` and maps
magpie row `r > H` to `L + (r − H)`; `before` pages come from LU; the conversion to magpie's epoch
(`agent.timeline.replacement`) is sent only while the tab does not show the conversation. A pigeon tab therefore never
sees an in-place re-render of a conversation it shows (B's certain R4 does not exist here).

### 7.3 The move rule (old-path sockets through the observer, proxy sockets in pigeon)

A socket is closed by the switch only when all hold, checked by the importer and re-checked by the closing component
at the instant of the close:
1. moves are armed (after the flip is live);
2. its last heartbeat said `appVisible: false`, is at most 20 s old, and it has been hidden for at least 30 s;
3. it has no outstanding request id and no inbound message other than `client_heartbeat` in the last 5 s (F2);
4. none of the person's conversations has an old turn running or held (the reconnect would be invisible anyway, but
   a running reply is where a defect would cost most);
5. for a proxy socket: the person is S2 (A §6.6).
The tab reconnects with its own 1.5 s backoff while still hidden; when the person looks again nothing has changed.

### 7.4 pigeon rollouts during the window

As A §6.7: allowed (resume by seq through LU or the anchor); the runner freezes routine pigeon rollouts from T0 to A5
except fixes; hidden-first drain.

---

## 8. The per-conversation hand-over

### 8.1 The rule

As A §7.1: gate set; person in S3; conversation in no old child's running list in two consecutive reads started
after its last ticket was done or expired; no old sandbox row not `gone` (the importer waits for the old janitor's own
30 min idle release and never releases a warm old sandbox itself); pre-verified and still matching. The last
condition also covers JuiceFS write-back: the old sandbox host's client has unmounted (sandbox `gone`) before the
cluster-side re-stat, so the cluster sees every byte the turn wrote.

### 8.2 Why the old stack cannot write after release

As A §7.2.

### 8.3 Fence and release

As A §7.3 (fence txn → re-stat → release txn, p99 ≤ 300 ms; a changed stat unfences; a send during the fence waits in
the ticket call and becomes the first native turn at `L + 1`; prefer releasing conversations no tab shows), plus the
upload pass of 4.4 inside the release transaction.

### 8.4 After the release

As A §7.4, plus F6: the first new-stack turn of a released conversation finds a warm hermit host (warm hints from the
importer), so it starts no slower than the old stack would have after the same idle pause.

---

## 9. Running turns and asks

### 9.1 Why nothing crosses live

As A §8.1. B's live adoption (agent v2, remote stream-json controller, hermit `legacy` kind) is not built (22).

### 9.2 A running old turn in the window

As A §8.2: old-path tab unchanged; proxy and native tabs get the old child's frames through LU in real time; Stop goes
to the old child (ticket → LU `abort`); holds and CLI follow-ups keep the conversation in the running list; bounds are
the old system's own (30 min hold ceiling, 8 h sandbox lifetime). No `legacy.cut`.

### 9.3 Asks (corrected, C1)

An old turn that calls AskUserQuestion or ExitPlanMode waits, exactly as it does today, because the old stack never
offered an answer path to the Paseo apps (1.2). Through LU every tab shows the turn as running and can Stop it, as
before the switch. The conversation counts as running, so it is never handed over mid-ask. After release, asks are
magpie's native flow (relayed and answerable), which is a change for the better, not a gap. B's re-published-ask
machinery has nothing to preserve and is not built.

### 9.4 New writes while an old turn runs

As A §8.4.

---

## 10. Attachments, sent files, files panel, shares, warm

### 10.1 Uploads: the tee plus legacy-path resolution

- After T0 every upload reaches pigeon (old-path tabs' HTTP follows the DNS). For a person with any legacy-owned
  conversation pigeon tees it to magpie M13 and to the old child through LU HTTP and answers with the old child's path
  and magpie's id, recording `uploads.legacy_path` (as A §9.1).
- Before T0 uploads went to the old child only. The importer's ingestion (4.4, F3) has every such file in
  `import_uploads` by A3 and re-runs the person's folder inside each release.
- At send time any old path resolves: through the old child (old-path, proxy or LU send), or through
  `uploads.legacy_path` / `import_uploads` (native send after release). Images sent inline need nothing.
- The tee acknowledges only when both writes succeeded; its limits are the old route's (30 MB, 10 files), so an upload
  error is the one the app shows today. A magpie-side failure is a magpie outage (2-6 replicas on a synchronous
  cluster), not a switch effect (20 R7).

### 10.2 The rest

Sent files, files panel, shares (create and immediate open; `continue` by the viewer's state) and warm: as A §9.2-9.5.

---

## 11. View-as, lists, titles, creates, deletes

- View-as: as A §10 (pigeon verifies old-router tokens with the derived legacy key, re-signs LU calls with it, records
  sessions under the token's own `s`; no admin restarts view-as; key removed in A6 after the 12 h token lifetime).
- Lists, overlays, titles, creates, deletes, restores, the 30-day purge: as A §11.

---

## 12. Retiring the old stack

As A §12 (preconditions held 30 min: every conversation owned, zero fenced, zero old-path / proxy / LU sockets, no
old sandbox row not `gone`, no old egress call in 30 min; then old connectors 0, ui 0, old web/egress/tools 0,
`import-done`), each step a YYC commit applied by the workflow, with the tab mover items replaced by: stop the mover
loop, close the NetworkPolicy holes, stop `egress-export-jarvis`.

**A tab that never hides** (21 Q1): without Deyao's widening the old stack stays up, idle on existing nodes, until
that tab closes; the runner reports the person and how long. The importer still hands over every other person.

---

## 13. Rollback

As A §13 (dark phases: revert; A3 before any release: revert the switch commit, pigeon closes its sockets while
hidden, reverse-hand-over conversations born on pigeon; after releases and in P4: per-conversation reverse hand-over
behind pigeon, invisible; after P4: no rollback to the old stack). In addition: the router observer is reverted by
one ui rollout if it misbehaves before A3 (it is observe-only, so reverting loses nothing but the moves; A4 then waits
for natural closes).

---

## 14. Failure modes

As A §14, with these rows replaced or added:

| Failure | Effect | Response |
|---|---|---|
| A ui pod's observer report missing or stale | its sockets' owners unknown | every person counts as possibly S1 (5.4): no new fences anywhere; everything keeps working through LU and the old path |
| Observer frame reader errors on a socket | that socket `opaque` | its person stays S1; alert; bytes unaffected (the reader runs after the pipe write) |
| Observer closes a socket wrongly (a visible tab or one with a request in flight) | one reconnect toast, or one failed request | prevented twice: the importer's check and the router's own re-check at the instant of the close (7.3); counted (`switch_visible_closes`), alerts and disarms moves |
| Importer leader dies between fence and release | the conversation stays fenced | the fence has a 3 s lease; the other replica takes the lock, re-stats and either releases or unfences; waiting sends then go native or through LU (tested, 15.3) |
| pigeon replica with LU connections dies | its tabs reconnect (normal rollout behaviour) | cursors resume through LU or the anchor; LU reopens on the new replica with the person's newest token (C4) |
| magpie primary switchover during a fence | fence and release transactions wait | synchronous standby; ticket calls and fenced writes wait ≤ 3 s, then retry as for any magpie unavailability; nothing is lost |
| A tee half fails | the upload fails as the app shows today | 10.1 |

---

## 15. Verification: proving zero gaps

### 15.1 Pass criteria (gap recorder + counters, F4)

From 5 min before T0 to the end of A5, all of these are 0, measured live and alerted by the runner (which then
disarms moves and Discords Deyao):

| Counter | Source | Means |
|---|---|---|
| `switch_visible_closes_total` (a close by the switch of a socket whose last heartbeat was visible) | router observer, pigeon | no shown tab lost its socket |
| `switch_inflight_closes_total` (a close by the switch with an outstanding request id) | router observer, pigeon | no request failed by a move |
| `pigeon_timeline_replacement_shown_total` | pigeon (heartbeat focus at send time) | no shown conversation re-rendered |
| `pigeon_lu_errors_total` / LU requests ≤ 0.1 % and no LU error surfaced to a tab | pigeon | the bridge does not fail |
| `magpie_unknown_upload_total{legacy="true"}` | magpie | every held attachment resolved |
| `pigeon_view_as_refused_total{key="legacy"}` for unexpired tokens | pigeon | no view-as restarted |
| `magpie_fence_seconds` p99 ≤ 0.3, max ≤ 3 | magpie | sends inside a fence waited at most 3 s |
| `magpie_conversation_migrating_total` (a 409 that reached a tab) | pigeon | G1 cannot happen |
| `import_verify_mismatch_total` | importer | every release byte-exact |
| edge 5xx and tunnel errors for the host (Cloudflare analytics, 1-min buckets) | Cloudflare | nothing failed at the edge |

The instrumented web build's gap recorder (A §15.1 item 6) is the client-side proof in CI and rehearsals: zero
visible-state reconnects, zero resets or replacements while shown, zero rpc errors, zero refused writes, and the
rendered text of every conversation equals the expected text.

### 15.2 Legacy-in-a-box (nightly)

As A §15.2, with: the tab mover replaced by the real router observer inside the pinned ui image; the "ask" persona
asserting that the old turn waits and Stop from a new tab ends it (C1); two personas added: **pre-T0 attachment**
(file attached in an old tab at T0−5 min, tab hidden and moved, sent at A4 after release: the agent reads the file)
and **busy hidden tab** (a hidden tab with a slow request in flight is not moved until the request is answered).
Pass on three consecutive nights blocks A3.

### 15.3 CI tests

As A §15.1 items 1-4 and 6, plus:
- invariants 6 and 7 in the ownership property test;
- **fence-race fuzz** (F5): 1 000 sends at random instants in [fence − 1 s, release + 1 s] with jitter: every send
  accepted exactly once, never refused, user row dense;
- **crash cases** (F5): importer leader `kill -9` between fence and release; pigeon replica killed with LU sockets;
  magpie primary switchover during a fence; ui pod observer report stale for 30 s;
- **router observer**: `jarvis/test/router.test.mjs` cases of 4.2, plus recorded sessions from the LU rig replayed
  through the reader (fragmented frames, pings, a 1 MB message, close frames).

### 15.4 Measurement before the switch (A2, production, read-only)

The observer runs from A1 with moves disarmed for at least 7 working days: old sockets per hour and per person,
lifetimes, hidden fractions, longest visible stretch, how many would have been movable within 10 min, 1 h and 24 h of a
working-hours T0, opaque count (must be 0). This replaces every estimate of A4's length.

### 15.5 Production rehearsals (before A3)

| # | Rehearsal | Passes when |
|---|---|---|
| R0 | DNS flip of the test host between tunnels `yyc-ai` and `pigeon` with 20 open sockets held 1 h; if needed the ingress-rule variant (6) | zero closes |
| R1 | The bot identity holds every state at once (F7): a long old turn in A, a hold in B, an ask in C, an idle warm old sandbox in D, a file attached but not sent in E, a web tab on A, the iOS phone replica on B, an admin's view-as tab of bot. Per-person gate (`magpie import allow-handover --person bot@deyaochen.com`), moves armed for bot only. Screen recording of both tabs plus the counters | every 15.1 counter 0; A finishes on the old stack and hands over; E's file reaches the agent; view-as continues; C ends by Stop |
| R2 | Release of a bot conversation while a visible tab shows it; remap; continued turn on magpie; later conversion while hidden | no replacement while shown; seqs dense |
| R3 | Reverse hand-over of a bot conversation and forward again | nothing visible |
| R4 | Load: 10 parallel synthetic conversations (cutover rehearsal 11) with bot's hand-over in the middle | counters 0 |
| R5 (optional, Deyao decides) | one consenting admin is handed over a day before A3 and works a normal day | counters 0 |

### 15.6 Live checks on switch day

As A §15.5 (state print every 5 s, alerts), with the counter set of 15.1.

---

## 16. Phases and the switch day

### 16.1 Phases

| Phase | Work | Done when |
|---|---|---|
| A0 build and prove offline | cutover P0, plus pigeon `legacy/`, magpie legacy routes and tables with `import_uploads`, importer changes (4.4), router observer (claudecodeui), instrumented app build, LU contract rig, legacy-in-a-box nightly | every CI green; 15.2 green three nights running |
| A1 new stack dark; observer release | cutover P1, plus the NetworkPolicies and keys of 4.5; **one in-place `SCM_RIGHTS` router upgrade** (23 C1) carrying the observer, raised `MAX_USERS`+memory, purge-off, LU-aware eviction and hot-reloadable allowlist/admins/budget/models — no child restart, no epoch change, no socket close; the window-input freeze guard armed (C1/C8); from here ui and `pigeon-app` stay at that claudecodeui commit until A5 and no further ui rollout is allowed (C3, C1) | test host serves; LU contract against production old router passes for bot; in-place upgrade rehearsed (A0) held every socket and child; observer reports from both ui pods |
| A2 bulk import, measurement, rehearsals | cutover P2 (bulk import, verify, parity), upload ingestion running, 15.4 (≥ 7 working days), 15.5 | every rehearsal passes; opaque = 0 |
| A3 flip (weekday, working hours) | preflight (cutover 11.2 minus the "old connectors 0" items, plus: both observer reports fresh, `import_uploads` caught up, warm hints flowing, USD 10 budget headroom), `start-handover`, the switch commit, wait for the flip, arm moves, set the gate | 20/20 `server: pigeon/` |
| A4 hand-over | automatic: persons S1 → S4, conversations release as they go idle | every conversation owned |
| A5 retire old | section 12 | old stack at 0; `verify --all` green |
| P4 (no wait, Deyao 2026-11-07) | as cutover 10.4; rollback per 13 | as cutover |
| A6 delete | cutover P5 plus section 17 | P5 checklist plus 17 |
| P6 +30 days | as cutover | as cutover |

### 16.2 The switch day

T0 is any weekday working hour once A2 is done (decision 9): Claude runs the runner and watches 15.6; Deyao needs to
do nothing. Expected shape: persons with no old socket go straight to S3 and their idle conversations release within
one pass; conversations with a running turn or a warm old sandbox release after it ends or idles out (≤ 30 min after
the last turn); a person with a visible old tab moves the first time that tab is hidden. 15.4's measurement gives the
real numbers before T0.

---

## 17. What is deleted

As A §17, with: the tab mover, its DaemonSet and its pcap fixtures are never built; added:

| Where | Deleted | When |
|---|---|---|
| claudecodeui `jarvis/router.mjs` | the observer (with the whole router, DESIGN 10) | A6 (P5.8) |
| aviary magpie | `import_uploads` and the legacy-prefix resolution in M3/M7 | A6, with the other import tables |
| store `yyc` | `YYC_TOKEN_PIGEON_UI`, `YYC_TOKEN_MAGPIE_IMPORT_UI` | A6 |
| YYC | NetworkPolicy `jarvis/ui-from-yyc-ai` (with the namespace) and the `yyc-ai` egress rules to `ui.jarvis` | A5 (rules), A6 |

The P5 checklist adds: `git grep -n 'legacy/\|x-aviary-lu\|import_uploads\|legacy_fence'` in aviary empty outside
history.

---

## 18. Cost

### 18.1 Build (Claude working days, beyond today's cutover plan)

| Piece | Days |
|---|---|
| A's pieces without the tab mover (pigeon `legacy/` 6, magpie 2, importer 2, LU rig 3, legacy-in-a-box 4, gap recorder and personas 2, runner 1.5, measurement and rehearsals 2) | 22.5 |
| router observer (claudecodeui) and its tests | 2 |
| mover loop in the importer | 0.5 |
| upload ingestion and legacy-path resolution (F3) | 1 |
| outstanding-request tracking in pigeon (F2), counters (F4) | 1 |
| fence-race fuzz and crash cases (F5) | 1.5 |
| warm hints (F6) | 0.25 |
| Saved from today's plan (pigeon 7.10 retry, `legacy.cut`, synthetic import turn, `final` release, `cut-old-connectors`, `wait-handover`, ui drain step) | −3 |
| **Net extra** | **≈ 25.75** |

For comparison: A as written ≈ 23.5 net (with the tab mover's 4 days), B ≈ 40 net.

### 18.2 Running

The old stack runs through A4 on the nodes it already has; old sandbox hosts run only for old turns that would have
run anyway; one warm hermit host during A4 (already part of normal warm capacity); rehearsal model spend under USD 1.

---

## 19. Changes to DESIGN.md and the specs

As A §19, with "switch-A" read as this document, and:
- **DESIGN.md 1** (zero-downtime row) and **7.3**: stated gaps are the single gateway node only (and, until decision 8
  lands, the single-instance cluster `pg`); 7.3 item 3 removed.
- **DESIGN.md 9.3/9.4**: no 04:00 HKT anywhere; phases as 16.
- **DESIGN.md 11** GitOps: the yyc-ai workflow applies every switch step (decision 1).
- **spec-cutover.md** 7.1 importer: gains the mover loop, upload ingestion and warm hints (4.4); `holds.js` exec kept.
- **spec-magpie.md**: `import_uploads` and legacy-prefix resolution in M3/M7 (until the contraction release).
- **claudecodeui**: the router observer (4.2) as a temporary section of `router.mjs`'s header comment.
- **spec-platform.md**: NetworkPolicy table gains `jarvis/ui-from-yyc-ai` and the two egress rules (temporary); the
  R24 note about a tab mover in the privileged namespace is not needed.

---

## 20. Residual risks

| # | Risk | Effect if it happens | Mitigation |
|---|---|---|---|
| R1 | Cloudflare closes established WebSockets on the old tunnel when the DNS record moves | every open tab reconnects once at T0 (a visible gap) | gate R0 before A3; ingress-rule variant; if neither keeps sockets, A3 does not start and Deyao decides (6) |
| R2 | **The never-hidden tab** | either the old stack waits for it indefinitely (idle, no extra nodes) or, with Deyao's widening, one ~2 s toast on a screen untouched for 30 min | 21 Q1; 15.4 measures how often it occurs |
| R3 | **The observer release touches live old code** | a defect in the tap could break old sockets for staff | observe-only (reader after the pipe write, try/catch, opaque on error, bounded-prefix parse per C15); byte-identity and per-frame latency tests gate A1; the release is an **in-place `SCM_RIGHTS` upgrade that restarts no child and closes no socket** (C1), so it is not a reconnect gap; revert is the same in-place path. Fallback if the handoff fails: one accepted routine rollout or A's tab mover (21 Q2) |
| R4 | Visibility depends on the browser being honest | a covered window reports `visible`: no move, a longer A4; never a visible gap | conservative rule (7.3) |
| R5 | Relying on unchanged old behaviour (forwarded Access token accepted from in-cluster callers, one child per person across both routers, run replay on re-subscribe, the child kept alive by LU heartbeats) | a surprise would show as an LU error | read from the live code; LU contract rig against the pinned image; R1-R2 in production; any surprise blocks A3 |
| R6 | Two projections in one shown tab (old rows before `L`, pigeon's after it) until conversion | a rendering difference between two different rows, never a duplicate or a gap | render parity (cutover 10.2 item 6); the remap property test |
| R7 | Upload tee doubles the write path | a magpie-side failure fails an upload that the old stack alone would have taken | magpie is HA; the failure is the app's normal upload error; counted |
| R8 | The window is longer than the baseline's 35 min (hours, maybe days): both stacks, two egress budgets (≤ 1 min lag), both fabrics, the importer | an old-stack incident in the window behaves as it does today (not gap-free, but not caused by the switch) | budget headroom; old fabric is 0 hosts unless an old turn runs; runner's live state |
| R9 | ui and `pigeon-app` frozen from A1 to A5 (≥ 7 working days of measurement plus A4) | app fixes wait | keep A2 to its measured minimum; a `pigeon-app`-only fix ships without touching ui; an unavoidable old-side fix ships through the **in-place router upgrade** (C1), not an ordinary rollout — and if it cannot (it changes the child, not the router) it is a stated gap needing Deyao's OK (21 Q2), never filed as routine |
| R10 | Build size: ≈ 26 extra days, about 10 000 lines of migration-only code | — | everything is deleted in A6; the legacy-in-a-box nightly and per-person rehearsals are what make shipping it safe |
| R11 | A Postgres or JuiceFS incident in the window | hand-overs stall (safe); both stacks affected as any day | not a switch effect |

---

## 21. Questions for Deyao

1. **The never-hidden tab** (R2): after 72 h, may a socket that is visible but untouched for 30 minutes, with nothing
   running for its person and no request in flight, be moved (one ~2 s "正在重新連線…" toast, resume by seq), or
   should the old stack wait for that tab however long it takes?
2. **The A1 release path** (C1, R3): the observer ships in a router that upgrades **in place** (listening and child
   fds passed over `SCM_RIGHTS`, no child restart, no epoch change, no socket close), bundling every old-side change
   the window needs. If the in-place handoff proves unreliable in A0 rehearsals, the only alternatives are one
   accepted routine ui rollout at a chosen low-traffic moment (reconnects every open tab once — a stated gap under
   decision 4) or A's privileged packet-capture tab mover (no ui change, but a hostPID/NET_ADMIN/SYS_ADMIN DaemonSet
   on production nodes). This design picks the in-place observer and asks Deyao to accept the one routine rollout only
   as the fallback if it fails.
3. **Rollback after a release the old child has cached** (C14.1): a reverse hand-over of a conversation whose old
   child already holds the timeline cannot be made invisible with unchanged old code (the child serves its frozen
   in-memory timeline; forcing a rebuild means restarting the child, which resets its shown conversations). The A1
   router can carry a one-conversation reload hook to make it invisible; may this hook be added, or is a visible
   reconnect accepted for the rare post-release rollback?

---

## 22. What was taken from B, and what was not

| B piece | Taken? | Why |
|---|---|---|
| the router as the place every old request passes (B1 insight) | yes, as the observe-only router observer (F1) | exact per-person attribution and visibility without a privileged sniffer |
| a close rejects in-flight requests (B 7.1) | yes (F2) | turns into invariant 6 |
| `cloudcli-assets` ingestion and legacy-path resolution (B 8) | yes (F3) | closes A's pre-T0 attachment hole |
| zero-gap counters (B 14.1), SW6 and crash tests, warm hints, all-states rehearsal | yes (F4-F7) | better proof at little cost |
| per-person fence at the old router, WebSocket termination and splice (B3, B8) | no | changes the live old router's hot path; still leaves one in-place replacement per open conversation; A's hidden moves plus remap reach zero visible effect without it |
| live adoption (agent v2, CloudCLI handoff, adopter, hermit `legacy` kind: B1, B2, B5, B6) | no | the largest and most fragile part of B (≈ 20 days, its R1, R2, R8); with LU a running old turn is fully live and controllable, so letting it finish costs nobody anything |
| re-published asks (B 9) | no | the old stack never offered an answer path (1.2); there is nothing to preserve |
| DNS last | no | in A, the DNS flip already changes nothing for anyone (old sockets stay, new ones are bridged); gate R0 covers the one assumption it rests on |

---

## 23. Attack findings and their fixes

This section is binding and **overrides any earlier text it conflicts with** (notably the §4.2 "ordinary ui
release", §6.5/§7.2/§8.3 "`before` pages from LU" and "LU closed in S4", §9/§11 conversion "only while not shown",
§4.2 mover reading `ui-peers`, §3.5 egress export, and R3/R9's "as every deploy"). Every scenario in the attack set
is resolved by one of the clusters C1-C13 below; the disposition table at 23.14 maps each scenario to its cluster.
The design principle that resolves most of them: **during the window a legacy conversation is read from magpie's
verified import, and only writes go to the old child through LU.** This removes the dependence of shown tabs on a
live old process (its epoch, its heap, its liveness), which is the root of most gaps.

### C1. The single in-place A1 release; no process restarts in the window

Facts: a ui rollout drains then `stop()`s every child (`router.mjs` `drain()`, `ui.yaml` `JARVIS_UI_DRAIN_SECONDS=900`);
each child restarts with a new epoch (`paseo-timeline.ts`); the old sandbox agent kills Claude Code's process group on
`/proc` drop (`jarvis3 chat/sandbox/agent.mjs`). So any restart of a child reconnects its tabs and replaces every
shown conversation's timeline — a gap.

Fix:
1. **In-place router upgrade.** The A1 entrypoint (added in the same commit) execs the new `router.mjs` with the
   listening fd and every live child `/proc` connection fd passed over `SCM_RIGHTS`; the new process adopts
   `p.sockets`, `forwarded` and the child registry from a handoff unix socket. Children are never stopped, epochs
   never change, client sockets never close. Reverting the observer (13) and any unavoidable old-side fix use the
   same in-place path, so neither is a gap. If the in-place handoff cannot be made to work in A0 rehearsals, the A1
   release is a stated exception put to Deyao (21 Q2) — it is never filed under "ordinary rollout".
2. **One release carries everything.** The observer, `MAX_USERS`+memory (C2), LU-aware eviction (C2), purge-off (C9),
   hot-reloadable allowlist/admins/budget/models (C7, C11) all ship in the A1 commit. No second ui rollout is needed
   or allowed in the window.
3. **Window freeze on every input that rolls namespace `jarvis` (C8).** A workflow guard refuses any commit from A1
   to A5 that touches `jarvis-config`, the `yyc/secrets-rev` annotation, `ui.yaml`, `db.yaml`, the old cloudflared
   manifests, or schedules a node drain; the runner also checks continuously and refuses/holds A3 if a ui pod, an old
   cloudflared pod or the shared `pg` primary restarted after the observer release, or if a node drain is pending.
   The ui `PodDisruptionBudget` is set to `maxUnavailable: 0` for the window so node pressure cannot evict a ui pod.
4. **No HPA churn (C8/F12).** pigeon's HPA is pinned `minReplicas = maxReplicas` from T0 to A5 so scale-in never
   closes a socket; routine pigeon rollouts are frozen (7.4); any pigeon fix is a stated gap needing Deyao's OK.
5. Resolves: the two "A1 observer release" scenarios, "something restarts or kills an old child", "pigeon churn",
   "restarts of ui that do not come from claudecodeui", "ui rollouts in the window", "other restarts during the
   days-long window" (node/k3s/pg-HA/cloudflared). The pg HA move (decision 8) is done entirely **before A1 or after
   A6**, never in the window.

### C2. Child capacity and eviction sized for the window

Facts: `JARVIS_UI_MAX_USERS=4` per pod, 2 pods (`ui.yaml`); `#process` evicts the LRU child with `inflight===0`
regardless of attached sockets and otherwise returns `503` "busy" (`router.mjs` 898-899, 908-909); LU
`client_heartbeat` touches `lastActivity` (463) so any person with legacy state keeps a child alive all window; an
anchor `loadTimeline` reads a full history into the 512 MB heap.

Fix, all in the A1 release:
1. Raise `JARVIS_UI_MAX_USERS` and the pod memory to cover **every person with legacy state plus view-as viewers
   plus anchor/`before` readers at once** (sized from 15.4's measured concurrent-children count, with headroom; the
   staff set is small — `config.json` has 7 users + 1 admin today). Add replicas if one pod cannot hold the heap.
2. **Children holding an LU socket are exempt from eviction**, and `#evict` never stops a child that is serving an
   anchor/`before` read; if the cap is still hit the request waits rather than thrashing.
3. **Do not keep children alive needlessly.** With reads served from magpie (C3) a native socket opens LU only while
   it has a legacy *write* path live, and LU on a hidden native socket is closed; so children idle-stop normally
   instead of being pinned for days by every hidden tab.
4. **Anchors never force a full history load (F36).** For a conversation no socket holds in epoch `E`, `L` is derived
   from the sessions-row/transcript metadata without `loadTimeline` (no tab holds `E`, so no remap is needed); for
   one a socket does hold, the anchor fetch is serialised per child. 
5. Counters `pigeon_lu_503_total` and `ui_child_evicted_total` join the zero set of 15.1; the runner alarms on the
   router's "busy"/"evicted" log lines. 15.4 measures the concurrent children LU will cause before A3.
6. Resolves: "the child cap", "old child capacity", "ui child capacity", "the anchor from an LU tail fetch".

### C3. Reads of legacy conversations come from magpie, not the live old child

This is the central change. From A2 the importer has a verified byte-exact copy of every legacy conversation plus a
stored **seq map** (epoch `E`, last old seq `L`, row head `H`, and the in-memory tail capture of C12). So during the
window pigeon serves, for a legacy-owned conversation, **all reads from magpie**: the timeline tail, `before` pages
(mapped through the stored anchor to magpie rows ≤ `H`), `files`, `files/raw`, `sent=`, the files panel, and share
views. Only **writes** (send, stop, rename, delete, restore, warm, share create/revoke, the old upload copy) go to
the old child through LU, each under a ticket (C5). Consequences:
- **A native socket needs no cold child to read** (fixes "an S2/S3 person's native socket touches legacy state for
  the first time" and "old-fabric first read"): opening a legacy conversation, scrolling up, opening files/shares
  never waits on a `JARVIS_UI_START_SECONDS` cold start and never 503s, because none of them touch the old child.
- **Released conversations' `before` pages do not depend on any old process** (fixes "remap after S4", "a tab that
  stayed away the whole window", "late E cursor reset"): a held epoch-`E` cursor maps forward to magpie rows, and
  `before` maps back to magpie rows ≤ `H` through the stored anchor. §6.5/§7.2/§8.3's "`before` via LU" is replaced
  by this; LU is not kept alive for reads, and a child idle-stopping after S4 changes nothing a tab sees.
- The anchor and the seq map are kept in magpie (C10 retention), so a cursor that arrives weeks later maps instead of
  resetting.
- Resolves: F4, F5 (remap after S4), the two "devices away" `before`-page resets, "a tab that stayed away the whole
  window wakes holding epoch-E cursors".

### C4. Conversion decided from timeline subscription, two-phase, never on `appVisible:false` alone

Facts: `session-context.tsx` `onReplacement` → `requestTimelineReplacement` fetches the tail and replaces rows
**immediately** (verified), not on the tab's next open; the app has split panes (`workspace-layout-actions.ts`) but
the heartbeat carries only `focusedAgentId`; `agent.timeline.replacement` being allowed on `appVisible:false` or
`focusedAgentId≠c` drops rows for a conversation that is on screen in another pane or behind a sheet.

Fix:
1. **"Shown" = pigeon's per-socket timeline subscription** (`agent.timeline.set_subscription`), never `focusedAgentId`
   and never `appVisible` alone. pigeon converts a conversation only on a socket that is **not subscribed** to it.
   This covers split panes and a focused-but-hidden tab.
2. **With C3, conversion is rarely needed at all**: a shown conversation keeps its epoch-`E` view served from magpie
   through the remap for as long as the socket shows it; the replacement is sent only when the socket has dropped the
   subscription (the tab navigated away), or deferred to the tab's own next open of `c`.
3. **Two-phase, with a hold-down (C6).** Before any conversion or move, pigeon marks "will convert/move" and proceeds
   only if no visible/focus/subscribe frame arrived within a 2 s hold-down; a visible or focus heartbeat, or a new
   subscription, within 2 s after the action counts as a gap in 15.1 and disarms.
4. §6.5, §7.2, §9, §11 claims of "conversion only while the tab does not show c (focus/appVisible)" are replaced by
   this subscription rule; 6.5's assertion that the app defers the refetch to the next open is corrected — the app
   refetches at once, so the decision is entirely server-side.
5. A test asserts scroll position and loaded history survive a hidden-tab/second-pane conversion
   (`pigeon_timeline_replacement_shown_total` includes subscribed-but-not-focused).
6. Resolves: "the conversion is sent when appVisible:false", "conversion allowed when focusedAgentId≠c OR
   appVisible:false", "split panes", the conversion half of the view-as and parity findings.

### C5. The fence is correct: ticket-on-outcome, quiet period, fail-closed at the old stack

Facts: `paseo-connection.ts` answers `send_agent_message_response` right after `paseoHub.send()`, which only does
`socket.deliver({type:'chat.send'})`; the user row, `isProcessing` and the `jarvis.sandboxes` row appear later. A
"ticket done = delivered" close can let the importer's two running-set reads both come back empty and release a
conversation the old stack is about to write — a lost turn. Nothing in the old stack refuses a write to a released
conversation.

Fix:
1. **A ticket closes only on the turn's *outcome*** (turn started **and** `isProcessing`, or complete/failed) reported
   by the old child through the server tap, never on frame delivery.
2. **Quiet period before the fence.** A conversation is fenceable only after ≥ 60 s with no ticket and with the old
   child's `pendingUserMessages`/`preparing` empty for it (read through LU), in addition to the two running-set reads.
3. **Fence inside the old stack, fail-closed (the decisive fix).** Once moves are armed, the **router observer
   rejects every non-LU upgrade and every `send`/`rename`/`delete`/`restore` frame for a conversation id magpie says
   is owned or fenced** (the observer already parses type and conversation id; it now refuses, closing the frame with
   an rpc error, rather than only observing). So even a stray old-path socket that appears after release — via an HK
   Cloudflare PoP still routing to tunnel `yyc-ai`, a partial revert, or a NotReady pod (C? → C8/F47) — cannot write
   a released conversation.
4. **Settle before the gate.** The flip probe runs from an HK vantage (and allows for Cloudflare propagation) and the
   gate is set only after a fixed settle time with every probe agreeing, so no first-hop old upgrade can open after
   the observer reports 0. `S3` requires fresh reports from **every** pigeon replica and **every** ui pod generated
   after the person's S2 transition, plus a person-state generation pigeon acknowledges and the fence re-checks.
   Proxy-socket writes also take tickets.
5. **Re-check every fingerprint inside the fence (F45).** The release re-stats the cc files, the sessions-row
   fingerprint, `sent_fp`, `jarvis_failed_turns` (the text of failed sends), upload references and the share rows —
   not the row and files alone. The ticket-free condition includes old-side failed-turn writes.
6. Counters: `switch_lost_turn_guard_total` (a release blocked by the quiet-period/outcome check), `old_write_after_release_total`
   (observer refusals; expected 0 after settle). Added to the fence-race fuzz (15.3): the split-brain interleaving,
   the post-release old upgrade, the stale-presence-at-fence race.
7. Resolves: "split brain after a ticketed LU send", "stale presence at the fence", "an old-path socket appears after
   a conversation was released", "the release check after pre-verification re-stats … only".

### C6. Moves are two-phase, hidden-proven, and never applied to mobile/WebKit sockets

Facts: web sends **no** heartbeat on becoming hidden (`use-client-activity.ts`: `if (changed && visible)` only), so
"hidden" is up to 15 s stale (≈ 60 s under Chrome intensive throttling); frozen/discarded/suspended tabs and iOS
(Safari, WKWebView, the app after background) run no JS while hidden, so a server close is seen only on resume;
`deviceType` and `appVisibilityChangedAt` are in the heartbeat payload; the toast shows at once on any disconnect
(`sync-status.tsx`).

Fix (applies to both the observer close and pigeon's proxy/native close, replacing 7.3's rule):
1. Move a socket only if its heartbeat proves its timers run while hidden: **at least two periodic hidden heartbeats
   15 s apart, the latest < 20 s old**, judged against the tab's own observed cadence (C13 measures hidden cadence in
   A2 so throttled tabs are known).
2. **Never actively move `deviceType=mobile` sockets, nor web-on-iOS/WebKit user agents** — let them end naturally.
   On mobile, `appVisible:false` from AppState `inactive` is treated as **visible** server-side (deviceType mobile +
   `appVisibilityChangedAt`), because an inactive app behind a share sheet / Control Center / Face ID / call banner
   is still on screen and already-shipped builds cannot change. So iOS sockets are never moved or converted mid-sheet.
3. **Two-phase with hold-down** (C4 item 3): a "will move" marker, no close if any frame arrived in the hold-down,
   and a `move_reconnected_while_hidden` ratio recorded per moved socket. If the reconnect's first heartbeat has
   `appVisible:true` with `appVisibilityChangedAt` after the close, or the reconnect lands > 5 s after the close,
   it is counted as a visible gap (`switch_visible_closes_total`) and **moves disarm**.
4. The client gap recorder reports "became visible within N s of a switch close" and must read 0 in the production
   rehearsals R1-R5, not only in CI.
5. Resolves: "races between the heartbeat and the action", "a hidden tab cannot reconnect before the user returns",
   "iOS (native app and Safari PWA)", "a move of a hidden socket assumes the tab reconnects while hidden", "the tab
   becomes visible again while a move or conversion is under way", "moves of hidden tabs", "hidden desktop tabs under
   intensive throttling".

### C7. Durable monotonic per-person state; presence never demotes

Facts: 5.1 makes p S1 while any ui report is missing or older than 15 s; a report hiccup would hand an S2-S4 person a
proxy socket whose old child lacks their magpie-born and owned conversations — their work looks lost on one device.

Fix:
1. The socket kind is decided from a **durable per-person state in magpie** (`person_switch_state`, "ever native"
   latch), not from a live presence read. A missing/stale report may block **new fences** (fail-closed, 5.4) but
   never sends a person who has left S1 back to proxy. States are strictly monotonic (rollback walks them back
   explicitly, C11).
2. Resolves: "stale presence (5.1 says p is S1 while any report is missing)".

### C8. Enumerate ui pods from the Kubernetes API, all phases

Facts: the mover loop reads `/__jarvis/sockets` from `ui-peers`, which has `publishNotReadyAddresses: false`
(`ui.yaml` 145), so a NotReady/Terminating pod during preStop or the 900 s drain holds old-path sockets and children
but is missing from DNS — its person is miscounted S2/S3 while an old-path socket can still write.

Fix: the importer enumerates ui pods from the Kubernetes API (all phases, Terminating included), as `holds.js` does,
and treats any pod that exists without a fresh report as "possibly S1" (fail-closed). Combined with C5's old-stack
fail-closed fence, a socket on a drains­ing pod cannot write a released conversation. (The window freeze C1 also means
ui pods are not being drained.) Resolves: "the mover loop reads `/__jarvis/sockets` from every ui pod (headless
ui-peers)".

### C9. The old purge and purgeWake are off for the window

Facts: `router.mjs` `purgeWake` (665-667, 920) starts a child daily and `purgeExpiredDeletedSessions` →
`deleteOrArchiveSessionById(force)` → `onConversationDeleted` revokes shares, releases the sandbox and removes
`ws/<conv>`/`sent/<conv>` on JuiceFS (`jarvis-conversations.ts`) — it acts on **transcripts and files**, not only
rows (switch-A 7.2 was wrong). A conversation restored in pigeon still has the old `deleted_at`, so the old child
purges files out from under a conversation magpie owns, failing `verify --all`.

Fix: turn off both the per-child Recently-deleted purge and `purgeWake` in the A1 release (env flag) for the window;
never release a conversation whose old row has `deleted_at` without first clearing old-side purge eligibility; the
share pass ignores old-side changes to shares of released conversations; verify treats disabled-purge as expected.
Resolves: "the old router's purgeWake", "the old Recently-deleted purge runs after a release".

### C10. Retention: anchors, legacy-path mappings and old chunks outlived the clients

Facts: Access sessions last 720 h, tabs stay open for weeks, drafts persist attachments (`uploadToCloudCli` sets
`id = stored.path`, the old assets path). A6 dropping `import_uploads`, `uploads.legacy_path`, `legacy_anchors` and
old chunks turns a late `before` into a reset and a late held-attachment send into `unknown_upload`.

Fix: keep `legacy_anchors`, the seq maps, `import_uploads`/`uploads.legacy_path` and legacy-path **rewriting** for at
least the 720 h Access lifetime plus margin (target 90 days, or until no client presents an old path or epoch) after
A5 — decoupled from A6's deletion of the import machinery. Keep the old-build static chunks for 30 days after the last
old-build tab is seen. A late epoch-`E` cursor maps forward from magpie rows (C3); a draft attached before T0 and sent
after A6 resolves through the kept mapping (decision 4: no redone attachments). Resolves: "devices away for the whole
window", "tabs that are asleep or away … and drafts kept in storage".

### C11. View-as, admins, budget, models and allowlist: one source, hot-reloadable, frozen edits

Facts: the old router checks admins against its own `config.admins` frozen at A1 (C3 of §3); `router.mjs` sets a
view-as socket's `email` to the viewed person and `actor` to the admin. Two budget gates and two configs
(`config.json` vs `catalogue.json`/`egress.json`) drift during a days-long window; a mid-window allowlist/model/limit
edit applies to one stack only.

Fix:
1. The A1 release makes the old router's **admins, staff allowlist, budget limit and model list hot-reloadable**
   (watch the ConfigMap/secret without a restart), so edits during the days-long window reach the old stack without a
   rollout. A single source (read by both stacks) is preferred; failing that, these edits are **frozen A3-A5**,
   enforced by the runner, with a preflight diff of both configs.
2. **`observer.person = who.email` (the viewed person)**; the view-as socket counts as an old-path socket of **both**
   the viewed person and the admin, and is moved like any other (C6). A legacy-in-a-box persona covers an old-path
   view-as socket. So the viewed person is not handed over while an admin's view-as tab still reads the old child.
3. Resolves: "view-as" (both the frozen-admins and the attribution halves), "an admin's view-as tab on the old path",
   "two budget gates and two configs", the allowlist/model-edit half of the config findings.

### C12. Parity is a zero-difference gate; the old in-memory timeline is captured at release

Facts: the old child's timeline is **not** the JSONL — `paseo-hub.ts` loads history once then appends live stream
frames in memory (`protocol_error` rows, failed-send user rows, streamed items never hit the transcript). So a
conversion rebuilt from the JSONL import shows different rows than the tab showed. Titles with `name_source` NULL get
retitled by magpie M7. `paseoHub.send()` split-brain (C5) and edit-send superseded transcripts also diverge.

Fix:
1. **At release, capture the old child's full in-memory timeline through LU** and store it as the conversation's
   legacy rows, so a conversion (C4) or a late reload shows exactly what the tab showed. The remap and `before` pages
   (C3) serve these captured rows.
2. **Render parity is a zero-difference gate for A3**, not "allowed differences": the gap recorder asserts the
   rendered text of every imported conversation equals the expected text, including rows that **cross the boundary**
   (a tool call started before `L` and finished after it) and the updated-row/`after < L` cases of the remap (F46:
   the remap allocates a new tab seq per change, as the old projection does; `after` cursors with `s < L` are served
   from the captured rows up to `L`). DESIGN P2's "allowed parity differences" is overridden for the window.
3. **Titles (F41):** the importer maps legacy provisional names to a `name_source` that blocks titling (or sets
   `title_attempts = 3`), so magpie does not rename a long-standing conversation on its first post-handover message.
4. Resolves: "two projections in one shown tab", "the conversion and the remap rebuild the old part from the JSONL
   import", "conversion and S2 native sockets show magpie's projection", "the remap maps magpie row r to L+(r−H)",
   "titles after release".

### C13. Version skew, build determinism, and chunk retention

Facts: web tabs never reload on a ui deploy (only reconnect), Access sessions last 720 h and there have been ≈ 50 ui
deploys since 2026-10-01, so tabs on many old bundles are open; `clients.txt` covers only the live web commit and
TestFlight builds since 2026-10-22; D13 seeds `pigeon-app` from one image's chunks; Docker-built ui images are not
proven to produce the same chunk hashes.

Fix:
1. From A1 the observer records every `appVersion` and bundle-entry hash seen on old-path and pigeon sockets (it reads
   `hello` and chunk requests) during A2.
2. Add **every bundle commit ui served within the Access session lifetime (720 h; target 30 days)** to `clients.txt`
   (so pigeon's frame validators do not drop frames those older validators reject) and seed `pigeon-app` with the
   static chunks of **every** such image (so lazy chunks do not 404 → `reloadOnce`).
3. A CI gate proves build determinism: `ui` dist and `pigeon-app` dist from the same commit match byte-for-byte;
   checked in preflight. Keep old-build chunks 30 days (C10).
4. Resolves: "version skew", "web tabs loaded from ui images older than the one live at T0".

### C14. Rollback, reverse hand-over and budget mechanics

Facts and fixes:
1. **Reverse hand-over cannot rely on the old child rebuilding a cached timeline** (`paseo-hub.timeline()` is rebuilt
   only on process start or `session_removed`; `13.2`/`A 13.2` step 4 was wrong). The reverse is specified as its own
   importer state: re-open `import_sources` (or create a row for a magpie-born conversation), reset `released_at` to
   null so the importer resumes syncing it, and **reverse only into a child that has never loaded the conversation**;
   if an in-place reload hook for one conversation is added to the A1 router, it may reverse into a live child, proven
   in R3 with a tab showing `c` throughout. Without such a hook, a reverse after a release that the child already
   cached **is a stated gap** (it needs a child restart), named to Deyao, not claimed invisible. The reverse fence may
   last as long as the export; the export runs in a writer sidecar ready in advance, not a cold Job.
2. **Reverse export completeness:** the export writes shares made on the new side, failed-turn rows and titles
   generated after the last export, or these are listed as rollback gaps for Deyao. Per-conversation auto-memory
   (decision 6) is exported into a place the old stack does not share (or dropped, decided explicitly), and the
   importer's memory-file guard is changed to ignore files the export wrote and to block only the affected
   conversation, never the whole importer.
3. **Rollback behind pigeon keeps the bridge up:** A5 does not delete the import Deployment, the mover loop, the
   `ui.jarvis` NetworkPolicies, the observer or either egress copy until **P4 ends**; the P4 rollback is written as
   its own procedure. DNS is not reverted while any native socket or pigeon-served page exists; roll back behind
   pigeon only. **Every upload is teed for the whole window** (not only for people with a legacy conversation) so a
   DNS-revert or an S2 native tab holding a magpie-only upload never hits `unknown_upload` or a silently dropped
   attachment (`filterAttachmentsToUploadStore`); the old send path is made to refuse rather than drop unknown
   attachments.
4. **Budget double-count (F32).** `egress-export-jarvis` rows re-imported by `egress import-jarvis` get fresh
   `bigserial` ids and are counted twice; `jarvis.egress_log` has no `source` column. Fix with a **reviewed migration**
   that adds a marker column to `jarvis.egress_log` written by a dedicated **writer** role for the export (the import
   credential stays read-only), and the new egress computes the budget from its own rows plus a read of
   `jarvis.egress_log` **excluding** exported rows. A test asserts no row is counted twice. §3.5's one-way marker
   claim is replaced by this.
5. Resolves: "reverse hand-over (rollback) of a conversation the old child already has", "reverse export …
   auto-memory", "any rollback that needs the export", "A3 rollback … DNS back", "rollback in P4 … A5 deletes the
   import Deployment", "budget across the window", the two reverse-hand-over timeline-cache findings and "mechanics of
   the reverse hand-over".

### C15. Latency, lists, uploads, warm, AWS quota, superseded transcripts, observer cost

Smaller fixes, each closing one scenario:
- **Fence/write latency (F13, F31, F51).** Fence only conversations that **no tab shows and no tab has focused in the
  last N minutes** (presence + subscription), so a fence wait can only hit a non-interactive path. Keep a **cached
  ticket lease** per legacy conversation, renewed in the background, so a normal LU write needs no synchronous magpie
  round trip. Take the anchor **before** the fence (the conversation is idle, `L` stable) and echo the user row
  optimistically. Any synchronous wait over 300 ms counts as a gap event (15.1). The old-child copy of a teed upload
  is made asynchronous after magpie acknowledges (resolution then needs only `import_uploads`).
- **Warm racing a release (F14).** Put `/api/jarvis/warm` under a ticket; a warm of a conversation being fenced is
  treated as warm to **hermit**; after the fence, warm routes to hermit. The release re-checks for warm calls in the
  last 60 s and for old sandbox rows (C5).
- **List freshness on native sockets (F20, F42).** Open **one list-level LU subscription per person per pigeon
  replica** (not per socket) to feed `agent_update` overlays (running state, title, `updatedAt`) for legacy
  conversations, so the sidebar never lags an old-path tab; or keep a magpie-side running mirror from the importer's
  running-set reads. A list-parity check (order, titles, `updatedAt`, running) between the old child's list and
  pigeon's overlaid list gates S2.
- **Upload tee limits (F43).** Tee only uploads from a socket/tab that can still send to a legacy-owned conversation;
  make the old write best-effort when the target is native; apply the **union** of limits and accept on whichever side
  will receive the send.
- **Legacy-path resolution (F44).** M7 makes a fresh upload copy (a new blob ref) for a legacy-path send instead of
  requiring the upload be unattached, and ingests synchronously on a miss before refusing — matching CloudCLI's reuse
  of an asset path any number of times.
- **AWS quota both fabrics (F49-quota, F50).** Keep the old fabric's normal warm pool and demand hints until A5; add
  AWS vCPU headroom for both fabrics' peak to the A3 preflight; the runner alarms on `preparing` p99 vs the 15.4
  baseline.
- **Model at release.** Read the old child's current model (`paseo-hub this.model.value`) at release and store it on
  the conversation in magpie (it is per process, not per conversation), checked in R1.
- **Superseded transcripts / new schemas (F49).** Re-run `plan` continuously in the loop; define handling of
  edit-send superseded transcripts; block only the affected conversation on a memory file (with decision-6 mapping),
  never the whole importer; handle first-time `cc_u` schemas.
- **Observer frame-reader cost (F-perf).** Extract `type` and `requestId` from a bounded prefix (first few KB) without
  parsing the whole message; skip payloads above a threshold (multi-MB timeline pages, base64 inline images). A
  per-frame added-latency p99 benchmark in `router.test.mjs` gates A1.

### 23.14 Disposition map (every scenario → cluster)

| Scenario (lead words) | Cluster(s) |
|---|---|
| A1 observer release (4.2/16.1/R3) | C1 |
| Something restarts or kills an old child during A3-A5 | C1, C3 (LU-loss reads), C2 (OOM) |
| The child cap (MAX_USERS 4) | C2, C3 |
| An S2/S3 native socket touches legacy state first time | C3 |
| Remap after S4 | C3 |
| Conversion sent when appVisible:false even when focusedAgentId==c | C4 |
| Races between the heartbeat and the action | C6, C4 |
| A hidden tab cannot reconnect before the user returns | C6 |
| iOS (native app and Safari PWA) | C6 |
| Stale presence (5.1 S1 while report missing) | C7 |
| Mixed sockets in S2 (proxy + native coexist) | C16 (below) |
| pigeon churn in the window / HPA scale-in | C1 |
| Fence wait up to 3 s | C15 |
| Warm racing a release | C15 |
| Devices away the whole window; drafts | C10, C3 |
| Version skew; clients.txt; build determinism | C13 |
| View-as (frozen admins + observer.person) | C11 |
| Reverse hand-over of a cached conversation | C14 |
| Token expiry (C4) LU reopen 401 | C17 (below) |
| The list on native sockets without LU | C15 |
| Two projections in one shown tab | C12 |
| A1 observer release + revert + R9 (SCM_RIGHTS) | C1 |
| Restarts of ui not from claudecodeui (configmap/secrets-rev/PDB/node) | C1, C8 |
| Split brain after a ticketed LU send | C5, C12 |
| Stale presence at the fence + colo propagation | C5, C8 |
| Move of a hidden socket assumes reconnect while hidden | C6 |
| The tab becomes visible again while a move/conversion is under way | C4, C6 |
| Conversion and remap rebuild old part from JSONL import | C12, C3 |
| Rollback reverse hand-over (paseo-hub cache) | C14 |
| Split panes / several panels | C4 |
| Every write through pigeon→magpie→LU latency; magpie unavailable | C15 |
| Budget double-count (egress export re-import) | C14 |
| Two budget gates and two configs | C11 |
| ui rollouts in the window | C1 |
| ui child capacity + eviction | C2 |
| Anchor from an LU tail fetch loads full history | C2, C3 |
| Conversion allowed when focusedAgentId≠c OR appVisible:false | C4 |
| Moves of hidden tabs (mobile/WebKit) | C6 |
| Hidden desktop tabs under intensive throttling | C6, C13 |
| Share continue routes by viewer state | C18 (below) |
| Titles after release (name_source NULL) | C12 |
| Native list no synthetic import turn; lazy LU | C15 |
| Upload tee limits (old 30 MB vs magpie 50 MiB) | C15 |
| Attachment path import_uploads but M7 unattached-only | C15 |
| Release check re-stats only (failed_turns/sent_fp/uploads/shares) | C5 |
| Remap maps r→L+(r−H); tool_result updates; after<L | C12 |
| Mover loop reads /__jarvis/sockets (publishNotReady:false) | C8 |
| Admin's view-as tab S-state attribution | C11 |
| Window lasts days; importer assumptions checked once | C15 |
| Old-fabric capacity during A4 | C15 |
| Send during a fence waits + anchor fetch | C15 |
| Mechanics of the reverse hand-over (released_at, verify) | C14 |
| Conversion/S2 parity differences | C12 |
| A tab that stayed away the whole window (epoch-E cursors) | C3, C10 |
| Other restarts (node/k3s/pg-HA/cloudflared) | C1 |
| Web tabs from older ui images (chunk 404) | C13 |
| The old Recently-deleted purge after a release | C9 |
| The old router's purgeWake | C9 |
| An old-path socket appears after a conversation was released | C5, C8 |
| The observer's frame reader in the single event loop | C15 |
| Working-hours window with both fabrics (AWS quota) | C15 |
| The old child keeps the model choice per process | C15 |

Three clusters named only in the map:

**C16. Mixed sockets in S2 (proxy + native coexist).** While any proxy socket of the person exists, **creates and
list-affecting writes route through LU (old child)** — the person stays "S1 for creates" until S3 — so a conversation
started on the phone is born legacy and appears in the still-open desktop proxy tab's list. (The alternative, pigeon
injecting magpie-born entries into proxy list frames, breaks the pure-pass-through property and is not taken.) §11.3's
"a person cannot be in S1 and S2 at once" is corrected: it is about *creates*, and creates stay legacy while a proxy
socket exists. Resolves both "mixed sockets in S2" scenarios.

**C17. Token lifetime carried into move/reopen.** The old router checks `exp` on every upgrade and request; an
established socket keeps going past expiry, but a reopened LU connection or LU HTTP call with a token past `exp`
(720 h after last sign-in) gets a raw 401 before the edge asks for sign-in. Fix: pass the token's remaining lifetime
into the move and LU-reopen logic; **never close or reopen LU for a person whose newest token expires within the
window**; alert on LU 401s. With C3 (reads from magpie) the exposure is limited to writes, and C1's freeze means few
reopens. Resolves "token expiry (C4)".

**C18. Shares routed by where the share lives.** Route share view/continue/revoke by whether the **magpie row** exists
vs the old `cc_shared` row, not by the viewer's person state; `continue` waits for hermit's workspace clone; a revoke
through LU is reflected in magpie on the ticket's NOTIFY. Resolves "share continue routes by the viewer's person
state".

### 23.15 Counters added to the zero set of 15.1

`pigeon_lu_503_total`, `ui_child_evicted_total`, `old_write_after_release_total`, `switch_lost_turn_guard_total`
(informational), `move_reconnected_while_hidden` ratio (target 1.0), `pigeon_timeline_replacement_shown_total`
(now keyed on subscription, not focus), `budget_double_count_total` (test-only, must be 0), `observer_frame_latency_p99`
(gate, not zero), `lu_401_total`.

---

## 24. Rejected findings

Only one claim in the attack set is not a gap as stated; the rest are accepted and fixed in section 23.

- **"The app does not wait until the next time c is shown, as 6.5 claims" — the *claim in 6.5* is wrong, not the
  finding.** Verified in `session-context.tsx`: `onReplacement` calls `requestTimelineReplacement` immediately, which
  refetches the tail and replaces rows. The finding is correct; §6.5's assertion is corrected in C4 (the decision is
  server-side, by subscription, with a hold-down). Listed here only to record that the *code behaves as the finding
  says*, so no app change is needed to "make it wait"; the fix is entirely in pigeon's send-time gating.

No finding was rejected as a non-gap: every scenario describes a real way decision 4 ("no gaps") is broken by the
merged design as previously written, and each is closed by a binding change in section 23. Where a fix cannot make a
case invisible with unchanged old code — a reverse hand-over into a child that already cached the conversation
(C14.1), and the A1 release itself if the in-place `SCM_RIGHTS` handoff fails in rehearsal (C1.1) — the case is a
**stated exception put to Deyao** (21 Q2/Q3), not filed under "ordinary rollout".

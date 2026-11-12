# pigeon

The stateless gateway the web and iOS apps talk to (pigeon spec, `refactor/spec-pigeon.md`). `pigeon serve` runs it;
`pigeon --version` prints the build. Image: `images/pigeon/Dockerfile` (`ghcr.io/de0ch/pigeon:<sha12>`).

Environment: secrets only (spec 3.3) — `PIGEON_TOKEN_MAGPIE`, `PIGEON_TOKEN_HERMIT`, `PIGEON_TOKEN_MODEL`,
`PIGEON_VIEW_AS_KEYS`, `PIGEON_BLOB_KEYS` — plus `POD_NAME` from the downward API (`aviary_core::log::init_in_pod`).
Upstreams are the fixed names `http://magpie`, `http://hermit`, `http://model` and `https://<team domain>`
(`Upstreams::cluster`); the app bundle is read from `/srv/app`.

## Modules

| Module | Spec | What |
|---|---|---|
| `identity` | 4 | deployment facts (H1, every 60 s), Access token sources and RS256 verification against every team's JWKS (1 h cache; one fetch in flight per team, which concurrent lookups of an unknown kid wait for; an unknown-kid refetch, or a retry of a failed refresh or a team with no keys yet, at most every 30 s; keys of a team hermit adds are loaded on the next facts refresh), verified-token cache, hermit people answers (15 s, stale-on-error 300 s), first names, the request identity |
| `http` | 5.1, 5.2.3, 5.5, 5.6, 5.13 | the routing table in order, path normalisation, the apps' error shape, the pages pigeon writes, same-origin rules, the model list, warm hints |
| `static_files`, `shell` | 5.2 | the bundle (in-memory file list, `.br`/`.gz` siblings, cache rules) and the rewritten `index.html` |
| `signin`, `viewas`, `ops`, `uploads`, `blobs`, `shares` | 5.3-5.4, 6, 5.11, 5.7, 5.8-5.9, 5.10 | the REST routes beside the socket |
| `server` | 5.13, 11.2 | the 8080 listener: connection tracking, `connection: close` while draining, the transfer bound |
| `raw` | (Deyao, 2026-11-05) | the raw view, standalone with the admin's own Access sign-in (assertion, cookie or app header; no view-as token): `GET /raw/<id>` (the page; signed out it goes once through the sign-in route and back), `GET /raw/<id>/lines` (cells with owner and title from magpie's `GET /v1/admin/conversations/{id}/transcript`, `Aviary-Admin` set to the admin, audited by magpie as an `admin_read`), and for a view-as tab `GET …/sessions/<id>/transcript` (from `GET /v1/conversations/{id}/transcript`), `cells` (each transcript line as typed cells: user, assistant, thinking, tool call, tool result, system, compaction, interrupt, bookkeeping), the shell's button script |
| `paseo::compat` | 1.2 | the shipped clients' wire constants: the only file in the crate that may name them (source rule, platform 4.5) |
| `paseo` | 7 | the seams: `SocketHandler` (`/paseo-ws`, with its drain) and `RowProjector` (spec 7.8; also used by the share view) |
| `paseo::transport` | 7.1 | the upgrade (100 MiB messages, 1009 beyond), per-socket state, the ordered bounded outbound queue (16 MiB / 4 096 frames, 1013), pings every 30 s, the 200/s (burst 400) inbound rate, the large-message semaphore (3 over 8 MiB), the 60 s re-checks (4403) and the token expiry (4401), 10 000 sockets per replica and 64 per person (1008) |
| `paseo::handshake` | 7.2, 7.3 | `ping` / `pong` (before hello too), `hello` (protocol 1; else `hello.rejected` then 1002), session frames before hello ignored |
| `paseo::dispatch` | 7.3, 7.5, 6.4 | every client message: static replies inline, the rest as concurrent tasks that always answer; view-as refusals before any call; the delegate seam |
| `paseo::messages`, `paseo::pushes` | 7.2-7.6 | envelopes, `server_info`, the static replies, `DAEMON_CONFIG`, the push payloads (built once, the socket's subscription id spliced in) |
| `paseo::mapping` | 7.4, 7.7, 7.8 | the object projections and `PaseoProjector`: rows of every kind, the ported Paseo tool-call mapper (Apache-2.0 notice in the file), the 16 MiB cap |
| `paseo::strings` | 7.9 | the zh (HK) / en sentences by data code and the fixed strings |
| `feeds` | 8 | the replica's `Hub` (sockets, who watches which timeline, pollers, the 2 s list single-flight), `change_poller` (M12 per person: list pushes, joins, sandbox chip, asks, turn events handed on) and `timeline_poller` (M9 per conversation: rows in `seqEnd` order, turn events after their rows, one replacement per new epoch) |

## The socket (WP-17) and its seams

`App::new` installs `paseo::transport::PaseoSocket` and `paseo::mapping::PaseoProjector`; `NoSocket` and
`BasicProjector` remain for tests of the HTTP surface alone. The socket's drain (11.2 step 3) runs beside the listener's
in the one drain hook.

The seams in `feeds` (built for the switch window, WP-50, whose legacy upstream went in the A6 contraction):
- **Per-conversation owner switch**: `feeds::Owners` (installed with `App::with_owners`, or `Hub::new` behind
  `App::with_socket`; the default `NativeOnly` serves
  everything from magpie) names, per person and conversation, a `feeds::Delegate`. A delegated conversation's
  requests (every conversation-scoped message, by `agentId` or `workspaceId`) go to `Delegate::request` with the
  socket, and its timeline subscriptions to `Delegate::subscribe` / `unsubscribe` instead of a native poller. The hub
  calls `Owners` without holding its own lock, so an `Owners` may consult the hub; a native poller lingering from
  before a delegation sends nothing while the conversation is delegated.
- **Per-socket subscription tracking**: `Hub::timeline_subscribers(person, conversation)`, `Hub::conns_of(person)`,
  `Conn::timeline_agents`, `Conn::send` / `send_sub`.
- **The release**: `Hub::owner_changed(person, conversation, epoch)` sends one `agent.timeline.replacement` in magpie's
  epoch to every socket watching the conversation and (re)starts its native poller from the tail of that epoch, with
  that replacement counted as sent.
- **Request-free first**: `Hub::drain` closes sockets with no request in flight, no message arriving and nothing but
  heartbeats for 5 s (`Tuning::socket_quiet`) in the first half of the 15 s jitter; the rest in the second half once
  their requests have answered (at most 10 s) and their transfers have ended (a large send arriving, or read and not
  yet handled: up to the 300 s transfer bound from SIGTERM, spec 11.2 step 3 and 13). Each 1012 follows the frames
  already queued (the reply it waited for, rows); when the writer reaches it while a request is in flight or a message
  is arriving (a send that began while that queue was being written), it waits for them in the same way and follows
  their replies (`Conn::close_for_drain`). The pollers keep feeding open sockets until the drain ends, and the drain
  (so the process) ends only when the sockets are gone or at the transfer bound. A socket whose upgrade was in
  flight at SIGTERM is closed 1012 as it registers.

## Deviations (recorded)

- **The model picker** (Deyao, 2026-11-12): where hermit's deployment facts say `conversations.peoplePickModel`,
  the `me` route (5.4) says `modelPick: true` and `set_agent_model_request` sets the conversation's model (magpie M5
  `model`) after checking the id against the models route's list (`null` picks the list's default); an agent's
  `model` is its conversation's `pickedModel`, else the list's default. Elsewhere the request is refused and every
  agent shows the default.
- **`midTurn`** (2026-11-12): the `me` route (5.4) says `midTurn: true`: a text message sent during a reply is
  accepted (magpie M7 joins it to the running turn), so the app sends it at once; an app talking to a server that
  does not say so keeps such a message in its queue until the reply ends.
- **A question's own answer** (2026-11-12): each question of an `AskUserQuestion` ask goes to the app with
  `allowOther: true` (the app's card shows its text field only then; Claude Code's own always offers "Other"), and
  the key is removed again from the answer's `updatedInput`.
- **The shell's raw-view script** (spec 5.2.2 says every other byte is served as built): the signed-in shell carries
  `raw::toggle_script` before `</body>`, which shows a "Raw transcript" button only in a view-as tab whose address
  names a conversation (the app bundle is frozen from A1, so the button cannot live in the app).
- **Header-read and idle timeouts** (spec 5.13: 30 s / 120 s): hyper 1.12 runs its header-read timer whenever a
  connection waits for the next request head, idle keep-alive included, so the two cannot be set apart. pigeon sets
  one 120 s bound, the one that matters (cloudflared reuses a pooled connection for up to 90 s).
- **Share view streaming** (spec 5.10): magpie's answer is parsed whole (`magpie_api` has no row-by-row reader);
  the projection is streamed out row by row, so the projected timeline is never held whole.
- **Byte relays**: `magpie-api` gained `files_raw_relay`, `share_files_raw_relay` and `blob_relay` (the range and
  conditional headers go up, every status comes back), so 206/304/416 pass through as spec 5.8 says.
- **Cross-origin refusal body**: `{"success": false, "error": {"code": "CROSS_ORIGIN", …}}`, the apps' error shape of
  5.1, rather than 5.13's bare `{"code": "CROSS_ORIGIN"}`.
- **View-as with no valid sign-in**: a request whose own Access token is missing or invalid is `SignedOut` (401)
  even when it carries a view-as token; 6.2's refusal (`VIEW_AS_REFUSED`) applies to a signed-in caller.
- The 7.10 migration retry is not built (DESIGN 12 D14, D24).
- **The 404 page of 5.3.3** (`appScheme` null): 5.2.3 has no 404 row, so pigeon adds one in its style
  (未能使用 / 這項功能在這裏未有開放。, Not available / This feature is not available here.), as HTML, since the route
  is opened in the system browser rather than fetched.
- **Warm bodies** (spec 5.6): an absent body, `{}` or `"sessionId": null` is the new-chat hint; a `sessionId` that
  is not a conversation id, a body that is not a JSON object, or one over 4 KiB sends no hint.
- **Dot segments encoded twice** (spec 5.1): a segment that is `.` or `..` after a second decode (`%252e%252e`) is a
  404 on every path, and `/ops` forwards the checked path encoded again, so hermit sees exactly the path pigeon
  checked (no dot segment, `?` or `#` appears on the way).
- **Drain test at CI scale** (spec 18.3): the upload across a drain is 4 MiB at about 2 MiB/s with the drain
  0.5 s in, not 50 MiB at 2 Mbit/s (210 s). The image send across a drain runs in CI as 12 MiB over about 5 s with the
  request wait at 1 s (so the send outlasts jitter + request wait), and at the spec's scale as an ignored test:
  `cargo test -p pigeon --test transport a_40_mib -- --ignored --nocapture` (40 MiB at 2 Mbit/s, SIGTERM 10 s in,
  pigeon's own 15 s / 10 s / 300 s): on the shared build runner (2026-11-02) the send took 168.1 s, its reply came,
  then its 1012, and the drain ended 170.5 s after the send began.

- **Socket message types** (spec 18.1; DESIGN 12 D25): pigeon's frames are built as JSON in the app's schema shapes,
  not hand-written serde types checked with the `jsonschema` crate. A JSON Schema exported from zod checks less than
  the zod schemas and the zod-aot validators the shipped clients run (refinements, transforms and preprocessors do
  not export), and serde types for a few dozen of about 430 message types would be a second copy of the shapes to keep
  in step. The drift protection is the clients' own validators, on two paths: every frame of a running pigeon driven
  by the clients' own package, as sent, is validated against every client build (`testing/paseo-contract/test/
  pigeon.test.mjs`, below), and the frame corpus:
  every socket test (`tests/socket.rs`, `tests/transport.rs`) records every frame its sockets send and receive, and a
  shape missing from `testing/paseo-contract/fixtures/pigeon-frames.ndjson` fails that test (`tests/common/corpus.rs`;
  `cargo test -p pigeon --test socket merge_frame_corpus -- --ignored` adds the missing shapes). The paseo-contract
  job validates the corpus against every client build in `clients.txt`: server frames against the outbound
  validators, and the client messages pigeon reads (every type it handles is required to be in the corpus) against
  the inbound schema, so a client build that changes a message pigeon handles fails there. Frames a test sends on
  purpose as invalid go through `Ws::raw_invalid` / `session_invalid` / `rpc_invalid` and are not recorded.
- **The app's real reducer** (spec 18.1): `testing/paseo-contract/test/pigeon.test.mjs` drives a running pigeon
  (`tests/contract_server.rs`: this router and socket in front of the in-memory magpie model, with a control endpoint
  for magpie's side) with the newest web build's `DaemonClient` and feeds what it receives through the app's own
  `processAgentStreamEvent` and `processTimelineResponse` (only the zustand store is a stub): no catch-up fetch when a
  rename, list upserts, the sandbox chip, an ask and a tool row's update interleave with a turn's rows, and every row
  exactly once across a rewrite while connected and a retract while the socket was closed. The same file drives pigeon
  with the newest web and iOS builds' client packages and validates every frame as sent against every client build.
  The client package's WebSocket factory is the harness's `ws` 8.20.0, which sends the Access assertion as a header
  (the browser sends a cookie; Node's global WebSocket sends no headers).
- **MCP tool names**: `mcp__<server>__<tool>` is shown and mapped as `<tool>`, so old-server history and
  `mcp__tools__*` calls get the same label and detail (spec 18.1); the old bridge showed the full name with an
  `unknown` detail, so render parity (cutover 10.2 item 6) must compare MCP tool names by their tool part.
- **`sub_agent`**: the vendored mapper has no `sub_agent` branch (`Task` is `unknown`, as the bridge showed it); the
  port keeps that.
- **No 7.10 migration retry and no "being moved" sentence** (DESIGN 12 D14, D24).
- **Live frames in M9 (`liveAfter`)**: off in this release (R36), so `timeline_poller` sends neither `liveAfter` nor
  `liveTurn`. When live frames are turned on (WP-17/WP-50, magpie 21.2), every `liveAfter` must go with `liveTurn`,
  the `LiveState.turnId` the frame number counts in: magpie answers 400 for either alone (frames are numbered per
  turn) and `reset: true` for a `liveTurn` that is not the active turn. fake-magpie applies the same 400 before any
  scripted answer (`testing/fakes/tests/timeline_query.rs`).
- **Updated rows**: a tool row updated before the poll read it is sent once, with its latest `seqEnd` (M9 `after`
  returns rows by `seqEnd`); the app sees a gap and fetches, as spec 8.3 describes for joins.
- **The drain and large inbound messages**: tungstenite gives the socket whole messages only, so the listener reads
  each upgraded connection's bytes through a parser of the client's frame headers (`server::InboundBytes`,
  `server::WsFrames`: opcode, FIN and payload length, payloads skipped): a message is arriving from its first header
  byte until the reader has it whole. A socket with a message arriving, or with a large send (256 KiB or more) read
  and not yet handled (waiting for a large-message slot included), has a transfer: the drain waits for it up to the
  transfer bound, not the 10 s request bound. While a client sends one long frame it cannot send a pong, so inbound
  bytes count as alive at the ping check. A reader waiting for a large-message slot keeps reading pongs and pings;
  the client's next data message (or close) is kept for after the waiting one, and from then the reader reads
  nothing more, so the socket counts as alive without pongs for at most the transfer bound from that moment (a dead
  client is dropped at its missed ping, and does not hold the drain to its bound). A close asked for while waiting
  ends the wait.

## Throughput (spec 8.6)

`taskset -c 1-15 cargo test --release -p pigeon --test socket bench_fanout -- --ignored --nocapture` on the shared
build runner (16 CPUs), with pigeon alone on CPU 0 (spec 13: a replica's CPU limit is 1): its whole tokio runtime (one
worker, its blocking threads, the listener, every socket and poller) is pinned there, while the 5 000 client sockets
and the in-memory magpie run on the other 15. 5 000 sockets open in 2.0-2.2 s; 20 000 frames (a row every 0.5 s to
each of 20 conversations, each watched by 50 sockets: 2 000 frames/s of fan-out, with 100 change pollers and 20
timeline pollers running) reach 1 000 sockets in 10.0 s: latency p50 16.6 / 19.4 ms, p99 170.4 / 161.9 ms, max
208.7 / 201.1 ms over two runs (2026-11-02). The spec's target is p99 ≤ 300 ms. In CI, `fan_out_latency_guard`
(200 sockets, 500 frames/s, debug build) fails if a frame is lost or the p99 reaches 1 s.

## Tests

- `tests/http.rs`: spec 18.2 (Access, people, shell, routing, sign-in, view-as, ops, uploads, files and blobs,
  shares, same origin) against fake-hermit, fake-magpie, a model stub and two fake Access teams (keys from `openssl`
  per run).
- `tests/drain.rs`: readiness gates and spec 18.3's HTTP drain cases.
- `tests/socket.rs`: spec 18.1's socket cases against `tests/common/model.rs`, an in-memory magpie with both long-polls
  (handshake, dispatch, paging, timeline and stale epochs, live rows and `turn_completed` after the last row, joins
  without loss or duplicate, one replacement per rewrite, the sandbox chip, 1013 for a slow socket, the drain's
  request-free-first rule, sends, creates, cancels, renames, deletes, asks, view-as, 4401), the frame corpus, and the
  ignored benchmark.
- `tests/transport.rs`: the transport limits (a missed ping drops and unregisters the socket, 1009, 1008 at the 65th
  socket, 4403 on re-check, the inbound rate), the drain (rows keep streaming until each socket's 1012, the 1012
  after the reply, a large send arriving across the drain, one that begins while the queue ahead of its 1012 is
  written, and one waiting for a large-message slot; a dead client on a slot dropped), the WP-50 seams,
  the list cache per viewer without kept failures, and spec 19's metric names.
- `tests/contract_server.rs`: the pigeon `testing/paseo-contract/test/pigeon.test.mjs` drives (ignored; it serves until
  its stdin closes), with its control endpoint for magpie's side.
- `tests/parity.rs`: every tool call of the recorded CloudCLI bridge log, rebuilt from the transcript the recorder fed
  the bridge (`testing/paseo-contract/record/cloudcli-bridge-488a7736.tools.json`, `node record/extract-tools.mjs`),
  rendered as the bridge rendered it; and conversation A's whole history (a user row with an image and a file,
  reasoning, every tool row across its two seqs, assistant and error rows) projected row by row through
  `PaseoProjector` and compared, envelope and item, with the entry the bridge served at the same seq. pigeon's own
  differences are asserted as such: image URLs are signed blob URLs with a thumbnail (5.9), the error row is pigeon's
  sentence for magpie's code (7.9), and adjacent seqs form one `sourceSeqRanges` range (the same coverage). Real
  imported transcripts are not compared here: their rows came from the importer (WP-29, deleted in A6), and render
  parity over the imported set was cutover 10.2 item 6 (WP-41).

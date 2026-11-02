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
| `paseo::compat` | 1.2 | the shipped clients' wire constants: the only file in the crate that may name them (source rule, platform 4.5) |
| `paseo` | 7 | the seams: `SocketHandler` (`/paseo-ws`, with its drain) and `RowProjector` (spec 7.8; also used by the share view) |
| `paseo::transport` | 7.1 | the upgrade (100 MiB messages, 1009 beyond), per-socket state, the ordered bounded outbound queue (16 MiB / 4 096 frames, 1013), pings every 30 s, the 200/s (burst 400) inbound rate, the large-message semaphore (3 over 8 MiB), the 60 s re-checks (4403) and the token expiry (4401), 10 000 sockets per replica and 64 per person (1008) |
| `paseo::handshake` | 7.2, 7.3 | `ping` / `pong` (before hello too), `hello` (protocol 1; else `hello.rejected` then 1002), session frames before hello ignored |
| `paseo::dispatch` | 7.3, 7.5, 6.4 | every client message: static replies inline, the rest as concurrent tasks that always answer; view-as refusals before any call; the delegate seam |
| `paseo::messages`, `paseo::pushes` | 7.2-7.6 | envelopes, `server_info`, the static replies, `DAEMON_CONFIG`, the push payloads (built once, the socket's subscription id spliced in) |
| `paseo::mapping` | 7.4, 7.7, 7.8 | the object projections and `PaseoProjector`: rows of every kind, the ported Paseo tool-call mapper (Apache-2.0 notice in the file), the 16 MiB cap |
| `paseo::strings` | 7.9 | the zh (HK) / en sentences by data code and the fixed strings |
| `feeds` | 8 | the replica's `Hub` (sockets, who watches which timeline, pollers, the 2 s list single-flight), `change_poller` (M12 per person: list pushes, joins, sandbox chip, asks, turn events handed on) and `timeline_poller` (M9 per conversation: rows in `seqEnd` order, turn events after their rows, one replacement per new epoch) |

## The socket (WP-17) and the seams for the switch window (WP-50)

`App::new` installs `paseo::transport::PaseoSocket` and `paseo::mapping::PaseoProjector`; `NoSocket` and
`BasicProjector` remain for tests of the HTTP surface alone. The socket's drain (11.2 step 3) runs beside the listener's
in the one drain hook.

WP-50 plugs into `feeds`:
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
  their requests have answered and their arriving messages have been handled (at most 10 s). Each 1012 follows the
  frames already queued (the reply it waited for). The pollers keep feeding open sockets until the drain ends.
  `Conn::request_free` is the same test for the move.

## Deviations (recorded)

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
- **Legacy view-as tokens** (`PIGEON_LEGACY_VIEW_AS_KEY`, spec 6.2, DESIGN 9.5) are WP-50's (`src/legacy/`); the
  7.10 migration retry is not built (DESIGN 12 D14, D24).
- **The 404 page of 5.3.3** (`appScheme` null): 5.2.3 has no 404 row, so pigeon adds one in its style
  (未能使用 / 這項功能在這裏未有開放。, Not available / This feature is not available here.), as HTML, since the route
  is opened in the system browser rather than fetched.
- **Warm bodies** (spec 5.6): an absent body, `{}` or `"sessionId": null` is the new-chat hint; a `sessionId` that
  is not a conversation id, a body that is not a JSON object, or one over 4 KiB sends no hint.
- **Dot segments encoded twice** (spec 5.1): a segment that is `.` or `..` after a second decode (`%252e%252e`) is a
  404 on every path, and `/ops` forwards the checked path encoded again, so hermit sees exactly the path pigeon
  checked (no dot segment, `?` or `#` appears on the way).
- **Drain test at CI scale** (spec 18.3): the upload across a drain is 4 MiB at about 2 MiB/s with the drain
  0.5 s in, not 50 MiB at 2 Mbit/s (210 s).

- **Socket message types** (spec 18.1): pigeon's frames are built as JSON in the app's schema shapes, not
  hand-written serde types checked with the `jsonschema` crate. The drift protection is the frame corpus instead:
  every socket test (`tests/socket.rs`, `tests/transport.rs`) records every frame its sockets send and receive, and a
  shape missing from `testing/paseo-contract/fixtures/pigeon-frames.ndjson` fails that test (`tests/common/corpus.rs`;
  `cargo test -p pigeon --test socket merge_frame_corpus -- --ignored` adds the missing shapes). The paseo-contract
  job validates the corpus against every client build in `clients.txt`: server frames against the outbound
  validators, and the client messages pigeon reads (every type it handles is required to be in the corpus) against
  the inbound schema, so a client build that changes a message pigeon handles fails there. Frames a test sends on
  purpose as invalid go through `Ws::raw_invalid` / `session_invalid` / `rpc_invalid` and are not recorded.
- **The app's reducer is not driven** (spec 18.1's "the app's real reducer" case): no harness feeds pigeon's frames
  through `paseo/packages/app/src/timeline`. Not met; see the package report.
- **MCP tool names**: `mcp__<server>__<tool>` is shown and mapped as `<tool>`, so old-server history and
  `mcp__tools__*` calls get the same label and detail (spec 18.1); the old bridge showed the full name with an
  `unknown` detail, so render parity (cutover 10.2 item 6) must compare MCP tool names by their tool part.
- **`sub_agent`**: the vendored mapper has no `sub_agent` branch (`Task` is `unknown`, as the bridge showed it); the
  port keeps that.
- **No 7.10 migration retry and no "being moved" sentence** (DESIGN 12 D14, D24); a `conversation_migrating` answer
  is the unreachable sentence until WP-50's delegate routes such conversations.
- **Updated rows**: a tool row updated before the poll read it is sent once, with its latest `seqEnd` (M9 `after`
  returns rows by `seqEnd`); the app sees a gap and fetches, as spec 8.3 describes for joins.
- **The drain and large inbound messages**: tungstenite gives the socket whole messages only, so the listener counts
  the bytes read on each connection (`server::InboundBytes`): a socket with more than 256 KiB received past its last
  whole message is busy (a message arriving), and so is one whose message waits for a large-message slot, until its
  handler ends (bounded by the 10 s request wait).

## Throughput (spec 8.6)

`cargo test --release -p pigeon --test socket bench -- --ignored --nocapture`, on the shared build runner pinned to
two CPUs (`taskset -c 0,1`) that also run the 5 000 client sockets and the in-memory magpie: 5 000 sockets open in
1.8 s; 20 000 frames to 1 000 watching sockets in 10 s (2 000 frames/s): latency p50 21.3 ms, p99 73.4 ms, max
123.5 ms (2026-11-02). Unpinned (16 CPUs): p50 12.2 ms, p99 59.1 ms.

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
  after the reply, a large send arriving across the drain and one waiting for a large-message slot), the WP-50 seams,
  the list cache per viewer without kept failures, and spec 19's metric names.
- `tests/parity.rs`: every tool call of the recorded CloudCLI bridge log, rebuilt from the transcript the recorder fed
  the bridge (`testing/paseo-contract/record/cloudcli-bridge-488a7736.tools.json`, `node record/extract-tools.mjs`),
  rendered as the bridge rendered it.

# paseo-contract

The Node harness that checks Paseo frames against every supported client build (pigeon spec 18.1). The Paseo apps
(web and iOS, built from [DE0CH/claudecodeui](https://github.com/DE0CH/claudecodeui) `paseo/`) validate every server
frame and drop one that fails, so a server may only send what every installed client accepts.

## What is checked

For each client build in `clients.txt` (a claudecodeui commit) the harness fetches that commit's
`paseo/packages/protocol`, runs the commit's own zod-aot generator, bundles the package with esbuild and applies every
validator that build applies to a server frame:

- `WSOutboundMessageSchema` (zod), the schema of record;
- `validateWSOutboundMessage`, the generated zod-aot validator the shipped bundle runs (a frame on which zod and zod-aot
  disagree is reported too);
- `ServerInfoStatusPayloadSchema` on `server_info`, and `KnownStatusPayloadSchema` on the other `status` payloads it
  knows.

Frames a client sends (`dir: in`) are checked against `WSInboundMessageSchema`, so a driver's own frames are realistic.
Builds sharing a protocol package tree are built and checked once.

Each build runs under the runtime its own `paseo/package-lock.json` resolves (`lib/runtime.mjs`): the harness walks
the lockfile from the workspaces it builds (their `dependencies`, plus the protocol's zod-aot generator) through every
transitive dependency the way Node resolves them (a workspace's own `packages/<name>/node_modules` first), downloads
each tarball from the URL the lockfile records, checks the lockfile's integrity hash and unpacks it at the lockfile's
path; no install scripts run. Builds that resolve the same versions share one install, and clients on different zod
(or ws, semver, …) versions are validated side by side in one run. The harness's own `package.json` holds only its
tools (esbuild, and ws for the test server). The commit's zod-aot generator patches zod-aot in place, as the app's
build does; it patches a private copy inside the build directory. A loaded bundle checks that the zod it runs is the
version the lockfile resolves.

Builds are cached in `~/.cache/paseo-contract` (`$XDG_CACHE_HOME/paseo-contract`, or `$PASEO_CONTRACT_CACHE`), which
must not sit under any directory holding a `node_modules` (checked: a package missing from a lockfile is an error,
never a silent fallback to another copy). Concurrent processes may share it: every directory is built under a scratch
name and renamed into place, and when another process got there first its copy is kept. The claudecodeui commits are
fetched shallow and blobless from `package.json`'s `paseo.repository` (or `$PASEO_CONTRACT_REPOSITORY`, e.g. a local
clone).

## Files

| File | What |
|---|---|
| `clients.txt` | the client builds that may be connected: `<commit> <kinds> <commit time, UTC>`, sorted by commit time (kinds: `web`, `ios`, `web,ios`, `web-legacy`) |
| `validate-frames.mjs` | `node validate-frames.mjs [--clients FILE] [--commit SHA]... [--protocol-dir DIR] [--outbound-only] LOG.ndjson...`; exit 0 all valid, 1 a frame invalid, 2 setup error |
| `export-schema.mjs` | writes `schema/<newest Paseo commit>.json`: JSON Schema (zod `toJSONSchema`, input side) of the outbound and inbound unions, for pigeon's Rust type tests; `--check` fails when it is stale |
| `append-client.mjs` | `node append-client.mjs <commit> <web\|ios> [<commit> <kind>]... [--file F]`: inserts builds at their place by commit time (run by `.github/workflows/clients.yml`) |
| `ci/land.sh` | the land job of `clients.yml`: applies the tested patch on its base, pushes, replays it on a main that moved elsewhere, exit 3 when the list went stale |
| `lib/protocol.mjs` | `protocolForCommit` (the validators of a build), `protocolForDirectory`, `clientForCommit` (its client SDK and its `DaemonClient`), `appTimelineForCommit` (its app's timeline reducers), `readClients`, `newestClient` |
| `lib/runtime.mjs` | installs a build's runtime packages from its own lockfile |
| `fixtures/cloudcli-bridge-488a7736.ndjson` | a recorded frame log of the CloudCLI Paseo bridge at 488a7736, the web build deployed when it was recorded (`record/`); the suite checks the commit the file name names is listed |
| `fixtures/pigeon-spec-frames.ndjson` | frames pigeon sends that the CloudCLI bridge never does (`hello.rejected` `incompatible_protocol`, pigeon spec 7.2), which every client must accept |
| `fixtures/pigeon-frames.ndjson` | one frame of every shape pigeon sends, recorded by its socket tests (`crates/pigeon/tests/socket.rs`; a shape missing here fails them); validated against every client build |
| `fixtures/seeded-invalid.ndjson` | frames each client must reject (a type change, a new enum value, a missing field, a new message type …) |
| `record/` | the recorder of the bridge log: `record/record.sh <claudecodeui checkout, after npm ci> OUT.ndjson` |
| `test/` | the harness's own tests and pigeon driven by the apps' code (`test/pigeon.test.mjs`): `npm test` |

## The client SDK for drivers

pigeon's contract suite drives a socket with the apps' own client package (pigeon spec 18.1). `clientForCommit(sha)`
bundles `paseo/packages/client` of a client commit, with the same commit's protocol and relay packages, into one
module (ws, zod and the relay's crypto at the versions its lockfile resolves) and returns `{ sdk, node, runtime }`
(`sdk.createPaseoClient({ url })`, …). `newestClient("web").sha` is the newest *listed* web build and
`newestClient("ios").sha` the newest listed iOS build, by commit time; a driver covering every build loops over
`readClients()`. The newest listed web build is not necessarily the one being served: a build is listed when it is
built, before its deploy (or without one), and a rollback serves an older pin. A driver that needs the deployed build
takes its commit as an input, from the ui pin in the deploy repository. The suite checks that the newest web build's SDK
connects to a server speaking the bridge's handshake.

## pigeon driven by the apps' own code

`test/pigeon.test.mjs` starts a real pigeon in front of an in-memory magpie (`crates/pigeon/tests/contract_server.rs`:
`cargo test -p pigeon --test contract_server -- --ignored --exact --nocapture serve`, from the repository root, or the
command in `$PIGEON_CONTRACT_SERVER`; `none` skips the file) and drives it with:

- **the client packages**: the newest web and iOS builds' `paseo/packages/client` (`createPaseoClient`, with a
  WebSocket factory that sends the Access assertion and records every frame as it is on the wire) connect, list and
  subscribe to agents and workspaces, read a timeline, send, and receive a turn with a rename and the sandbox chip;
  every frame both ways, unnormalised, is validated against every client build in `clients.txt`;
- **the app's timeline reducers** (`appTimelineForCommit`: `paseo/packages/app/src/timeline/session-stream-reducers.ts`,
  `timeline-sync-plan.ts` and `types/stream.ts` of the newest web build, bundled with its protocol under its own zod;
  only the zustand store is a stub, which the pure reducers do not read), fed by the client package's own
  `DaemonClient` (`observeTimeline` updates through `processAgentStreamEvent`, `fetchAgentTimeline` answers through
  `processTimelineResponse`, a replacement refetching the tail, as `session-context` wires them). Two cases: a turn
  whose rows interleave with a rename, list upserts, the sandbox chip, an ask and a tool row's update renders exactly
  as a fresh tail fetch does, with no catch-up fetch; and a turn across a rewrite while connected (one replacement)
  and a retract while the socket is closed (the resume answers with a reset) keeps every row exactly once and drops
  the retracted ones. A dropped live row makes the first case fail (a catch-up fetch), which was checked by hand.

CI runs it in the `paseo-contract` job (which builds the contract server first) and in `clients.yml`, so a new
client build is also driven against pigeon before it is listed.

## The recorded bridge log

The bridge log is recorded in process from the bridge code of the deployed web build's commit (`record/`), not captured
from production traffic: a wire capture would hold staff conversations, and the harness may not touch the live
cluster. The recorder drives that commit's own `paseo-connection` / `paseo-hub` / `paseo-timeline` with an in-memory
session store and made-up conversations (product name, model and people are placeholders). It covers user rows with an
image and a file, assistant, thinking, error and tool rows (Bash, Read, Edit, Write, Grep, Glob, WebSearch, WebFetch,
TodoWrite, Task, SendUserFile, MCP tools, failed calls), live turns, failures, cancels, create / rename / delete /
restore, view-as refusals, an iOS-type client and a hello of a newer protocol (the bridge has no version gate and
answers `server_info`; pigeon's `hello.rejected` is in `fixtures/pigeon-spec-frames.ndjson`).

## Frame logs

NDJSON, one frame per line: `{"dir": "out" | "in", "frame": {...}, "case"?: "...", "expect"?: "invalid"}`. A line
without `dir` is a bare server frame. `expect: invalid` marks a frame outside the schema on purpose (a test of the
server's dispatch rules, such as an unknown request type); it must fail validation. pigeon's contract suite writes
every frame it saw in this format to `pigeon-frames.ndjson`, published with each release, and the app's workflow
validates that corpus against a new app commit before publishing it (`--protocol-dir paseo/packages/protocol`).

## Adding a client

The app's workflows send a `repository_dispatch` of type `paseo-client` to this repository, with `{"commit": "<40
hex>", "kind": "web" | "ios"}` for one build or `{"clients": [{"commit": …, "kind": …}, …]}` for several (a seed list
goes as one dispatch). `clients.yml` checks each commit is on claudecodeui main, inserts the lines at their place by
commit time (builds finish out of order: a late TestFlight upload of an older commit does not become the newest),
re-exports the schema of the newest Paseo client, runs this suite on the result, and only then lands it on main.

- **No build is dropped.** The workflow has no concurrency group: GitHub keeps one pending run per group and cancels
  the pending one when another arrives, and `repository_dispatch` answers 204 either way, so a group would lose builds
  in a burst without a trace. Runs go in parallel; the first to push wins. A run whose tested list went stale
  (`testing/paseo-contract` changed on main after it started) dispatches `clients.yml` again with the same builds
  (`workflow_dispatch`, input `attempt`, up to 20), which redoes the append on the new main; a move of main elsewhere
  is replayed under the tested commit. So every dispatch ends with its builds listed, or with a red run. The suite's
  burst test runs five parallel appends against a local repository through `ci/land.sh` with an unrelated push in
  between, and checks every build is listed.
- **Trust.** The build job runs code from the client commits (their zod-aot generators; the suite imports their
  packages), so it has a read-only token and keeps no credentials in its checkout; it hands its change on as an
  artifact (a patch and its base). The land job, which holds `contents: write`, runs no client code: a fresh checkout
  of main, `ci/land.sh` from that checkout, the patch applied (it may touch only `testing/paseo-contract`), git hooks
  off, a plain push (never forced). What the build job's client code can affect is the patch's content, which is the
  tested list and schema it produced; the commits are checked to be on claudecodeui main first.
- **CI on the new main.** A push made with the workflow's token starts no workflow, so the land job starts `ci.yml` on
  main itself (`gh workflow run`, `actions: write`): when a new newest client changes `schema/<commit>.json` for a
  message pigeon handles, pigeon's Rust schema tests fail as a red check on main (pigeon spec 18.1).

**Pre-Paseo ui images.** cutover 13.1, DESIGN 8.3/9.6 and WP-27 list every ui image served within the 720 h Access
session lifetime. The images pinned from 2026-10-21 to 2026-10-23 come from claudecodeui commits without `paseo/`
(Paseo first appears in 3968b730, 2026-10-26): they served the CloudCLI web UI and have no protocol to validate. A
`web` dispatch of such a commit (older than 3968b730) is listed as `web-legacy`, which the validators and the schema
export skip, so `pigeon-app` (WP-27) can dispatch every seeded image as specified and `clients.txt` stays the one
list. An `ios` dispatch, or a newer commit, without `paseo/` is refused.

A build leaves `clients.txt` only when no connection with its version has been seen for 30 days (pigeon spec 1.2 step
3, cutover 13.1); a `web-legacy` line, 720 h after its image stopped being served.

## Running

```sh
cd testing/paseo-contract
npm ci
npm test                                                  # the harness's acceptance (a simulated append, a burst)
node validate-frames.mjs fixtures/cloudcli-bridge-488a7736.ndjson
```

Node 22, network access to GitHub and the npm registry. A cold run installs each distinct runtime and builds each
distinct protocol package (about 20 s each); later runs use the cache.

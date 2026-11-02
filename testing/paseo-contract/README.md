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

Each build runs under the harness's one installed version of each runtime package (`zod`, `zod-aot`, `semver` for the
protocol package; `ws`, `tweetnacl`, `base64-js`, `@noble/*` for the client and relay packages), pinned in
`package.json` at the versions the app's lockfile resolves. A client commit whose own `paseo/package-lock.json`
resolves another version of one of them is refused (exit 2) rather than validated under the wrong runtime: bump the
pin here, with its lockfile, to match. The commit's zod-aot generator patches zod-aot in place, as the app's build
does; it patches a private copy inside the build directory, never the harness's `node_modules`. Builds are cached in
`node_modules/.cache/paseo-contract` (or `$PASEO_CONTRACT_CACHE`); the claudecodeui commits are fetched shallow and
blobless from `package.json`'s `paseo.repository` (or `$PASEO_CONTRACT_REPOSITORY`, e.g. a local clone).

## Files

| File | What |
|---|---|
| `clients.txt` | the client builds that may be connected: `<commit> <kinds> <commit time, UTC>`, sorted by commit time (kinds: `web`, `ios`, `web,ios`, `web-legacy`) |
| `validate-frames.mjs` | `node validate-frames.mjs [--clients FILE] [--commit SHA]... [--protocol-dir DIR] [--outbound-only] LOG.ndjson...`; exit 0 all valid, 1 a frame invalid, 2 setup error |
| `export-schema.mjs` | writes `schema/<newest Paseo commit>.json`: JSON Schema (zod `toJSONSchema`, input side) of the outbound and inbound unions, for pigeon's Rust type tests; `--check` fails when it is stale |
| `append-client.mjs` | `node append-client.mjs <commit> <web\|ios> [--file F]`: inserts a build at its place by commit time (run by `.github/workflows/clients.yml`) |
| `lib/protocol.mjs` | `protocolForCommit` (the validators of a build), `clientForCommit` (its client SDK), `readClients`, `newestClient` |
| `fixtures/cloudcli-bridge-488a7736.ndjson` | a recorded frame log of the CloudCLI Paseo bridge at the live web build's commit (`record/`); the file name names the live web build, which the suite checks is listed |
| `fixtures/pigeon-spec-frames.ndjson` | frames pigeon sends that the CloudCLI bridge never does (`hello.rejected` `incompatible_protocol`, pigeon spec 7.2), which every client must accept |
| `fixtures/seeded-invalid.ndjson` | frames each client must reject (a type change, a new enum value, a missing field, a new message type …) |
| `record/` | the recorder of the bridge log: `record/record.sh <claudecodeui checkout, after npm ci> OUT.ndjson` |
| `test/` | the harness's own tests: `npm test` |

## The client SDK for drivers

pigeon's contract suite drives a socket with the apps' own client package (pigeon spec 18.1). `clientForCommit(sha)`
bundles `paseo/packages/client` of a client commit, with the same commit's protocol and relay packages, into one
module and returns `{ sdk, node }` (`sdk.createPaseoClient({ url })`, …). The live web build is
`newestClient("web").sha`, the newest iOS build `newestClient("ios").sha`; a driver covering every build loops over
`readClients()`. The suite checks that the newest web build's SDK connects to a server speaking the bridge's handshake.

## The recorded bridge log

The bridge log is recorded in process from the bridge code of the live web build's commit (`record/`), not captured
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

The app's workflows send a `repository_dispatch` of type `paseo-client` with `{"commit": "<40 hex>", "kind": "web" |
"ios"}` to this repository. `clients.yml` checks the commit is on claudecodeui main, inserts the line at its place by
commit time (builds finish out of order: a late TestFlight upload of an older commit does not become the newest),
re-exports the schema of the newest Paseo client, runs this suite on the result, and only then pushes to main (a plain
push; if `testing/paseo-contract` changed on main meanwhile the run fails instead, and a re-run redoes it). The job
keeps no credentials in its checkout; the token reaches only the push step.

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
npm test                                                  # the harness's acceptance (includes a simulated append)
node validate-frames.mjs fixtures/cloudcli-bridge-488a7736.ndjson
```

Node 22. A cold run builds each distinct protocol package (about 20 s each); later runs use the cache.

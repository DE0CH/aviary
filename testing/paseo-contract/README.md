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
Builds sharing a protocol package tree are built and checked once. Builds are cached in
`node_modules/.cache/paseo-contract` (or `$PASEO_CONTRACT_CACHE`); the claudecodeui commits are fetched shallow and
blobless from `package.json`'s `paseo.repository` (or `$PASEO_CONTRACT_REPOSITORY`, e.g. a local clone).

## Files

| File | What |
|---|---|
| `clients.txt` | the client builds that may be connected: `<commit> <web\|ios\|web,ios> <date>`, oldest first |
| `validate-frames.mjs` | `node validate-frames.mjs [--clients FILE] [--commit SHA]... [--protocol-dir DIR] [--outbound-only] LOG.ndjson...`; exit 0 all valid, 1 a frame invalid, 2 setup error |
| `export-schema.mjs` | writes `schema/<newest commit>.json`: JSON Schema (zod `toJSONSchema`, input side) of the outbound and inbound unions, for pigeon's Rust type tests; `--check` fails when it is stale |
| `append-client.mjs` | `node append-client.mjs <commit> <web\|ios>`: adds a build (run by `.github/workflows/clients.yml`) |
| `fixtures/cloudcli-bridge-488a7736.ndjson` | a recorded frame log of the CloudCLI Paseo bridge at the live web build's commit (`record/`) |
| `fixtures/seeded-invalid.ndjson` | frames each client must reject (a type change, a new enum value, a missing field, a new message type …) |
| `record/` | the recorder of the bridge log: `record/record.sh <claudecodeui checkout, after npm ci> OUT.ndjson` |
| `test/` | the harness's own tests: `npm test` |

`package.json`'s `paseo.client` is the commit of the live web build (the newest client); drivers that speak to a server
with the app's own client package (`paseo/packages/client`) use that commit.

## Frame logs

NDJSON, one frame per line: `{"dir": "out" | "in", "frame": {...}, "case"?: "...", "expect"?: "invalid"}`. A line
without `dir` is a bare server frame. `expect: invalid` marks a frame outside the schema on purpose (a test of the
server's dispatch rules, such as an unknown request type); it must fail validation. pigeon's contract suite writes
every frame it saw in this format to `pigeon-frames.ndjson`, published with each release, and the app's workflow
validates that corpus against a new app commit before publishing it (`--protocol-dir paseo/packages/protocol`).

## Adding a client

The app's workflows send a `repository_dispatch` of type `paseo-client` with `{"commit": "<40 hex>", "kind": "web" |
"ios"}` to this repository; `clients.yml` appends the line, re-exports the schema of the newest client, pushes to
main and runs this suite. A build leaves `clients.txt` only when no connection with its version has been seen for 30
days (pigeon spec 1.2 step 3, cutover 13.1).

## Running

```sh
cd testing/paseo-contract
npm ci
npm test                                                  # the harness's acceptance
node validate-frames.mjs fixtures/cloudcli-bridge-488a7736.ndjson
```

Node 22. A cold run builds each distinct protocol package (about 20 s each); later runs use the cache.

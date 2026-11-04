# Test fakes (WP-08)

In-process fakes for the other crates' tests (`aviary-fakes`) and the standalone fakes the harness runs
(`mock-model`, `fake-tools`, `fake-access`; platform spec 7.4-7.6).

## aviary-fakes

`FakeMagpie`, `FakeHermit` and `FakeSupervisor` implement the `Server` traits of `magpie-api`, `hermit-api` and
`supervisor-api` (`ControlServer` and `LocalServer`) and are served by those crates' own routers, so paths,
extractors and status codes are the contract's. All three check the caller first, before any scripted answer:
hermit through hermit-api's guarded router, magpie with a generated pair token per caller and the route table's
`callers` (401 `unauthenticated`, 403 `wrong_caller`; `running.client(http, Caller::Gateway)` speaks as a caller,
`FakeMagpie::unguarded()` checks nothing), the supervisor's control routes (`exec` included) with the control token.

```rust
let magpie = FakeMagpie::new().serve().await?;          // a local port; magpie.url()
magpie.fake.script_fixture("get_conversation", 0);      // answer from the golden fixture
magpie.fake.script().once("lease", Reply::error(409, "not_current_sandbox", "…"));
magpie.fake.faults().times("cc_write", FaultKind::Unavailable { retry_after_secs: 1 }, 3);
let calls = magpie.fake.script().wait_calls("heartbeat", 2, Duration::from_secs(5)).await;
```

- **Scripts** (`script.rs`): per route (the trait method name, which for magpie is also the route id) an answer
  given once, a handler that sees the call, or an answer given every time, tried in that order. Every call is
  recorded (path parameters, parsed query and body, raw bytes, headers).
- **Models**, used when nothing is scripted:
  - magpie: every `/v1/sandbox/**` route (lease and epochs, `not_current_sandbox`, `lease_lost`; `~/.claude`
    files and directories with magpie 6.6 rule 0 (a write of bytes already in place changes nothing), the
    `offset_beyond_eof`, missing-parent, `protected`, `exists`, `not_empty` and quota rules and `opId` replays, a
    refused operation leaving nothing behind; renames as `rename(2)`; the command queue with its long-poll and
    acks; blobs by sha256; SendUserFile snapshots, `sent-files` and its commit per `toolUseId`; reports
    accepted). The person, hermit and ops routes have no model and answer 501 `not_supported` unless scripted.
    The module docs list the deliberate differences (the lease with no current sandbox named; blob `GET` unscoped
    by conversation or lease, `as` ignored; the commit's `seq` and missing-blob code) and the codes only a script
    produces (`conversation_migrating`, lease 503, the per-line 413, `offset_mismatch`).
  - hermit: deployment, the catalogue rules, one sandbox per conversation (idempotent `PUT`, the long-poll `GET`,
    release, the YYC 501 for pause/resume/snapshot), the feed (`latest`, order, `limit`, long-poll, 410), signed
    sandbox tokens and their keys, liveness, the descriptor with etag and 304, secrets by grant, activity reports,
    workspaces. Starting sandboxes become ready after `HermitConfig::ready_after` or on `make_ready`.
  - supervisor: the control token, health and state from a settable run state, release refused while busy unless
    forced, prepare, resumed; the local socket's `send-user-file` and `secret`.
- **Faults** (`fault.rs`), per route name or path template, or everywhere (unmatched paths included), for N
  requests or until removed: `Latency`, `Unavailable` (503 with `retry-after`), `Hold` (until released; `wait_held`
  to synchronise), `Drop` (the connection closes with no answer; needs `serve`, whose accept loop can drop a
  connection; elsewhere it answers 500 saying so).
- **Fixtures** (`fixtures.rs`): the contract crates' golden fixtures, embedded at build time (`build.rs`), so the
  helpers also work from a built binary or image.

The fixture tests (`tests/*_fixtures.rs`) send every golden fixture of the three contract crates through the
contract clients against the fakes; `tests/faults.rs` exercises each knob; `tests/models.rs` the models.

## mock-model

The Messages API and the models route, scripted with `POST /__script` (platform 7.5), taking request bodies up to
64 MiB (the gateway's `model` route limit, platform 3.11.3; above it the Messages API's 413 `request_too_large`).
Replies come in one of two framings (`--wire`, or `wire` on a script entry): `openrouter` (the default), the wire
form of the upstream YYC's gateway relays to, written field by field and checked byte for byte by `tests/mock.rs`
against the recordings in `testing/mock-model/recorded/`; or `anthropic`, Anthropic's native framing (`ping` after
`message_start`, the input usage in `message_start`, no `provider`/`cost` fields, `toolu_` ids), written from
Anthropic's streaming documentation and checked by its own test, with no recording behind it (see
`recorded/README.md`). A scripted thinking `signature` is streamed as a `signature_delta` in both. An unscripted
title request answers `測試標題`: one for the title model id, or, when the models file gives default, small and
title one id (as YYC's `egress.json` does), one shaped like the supervisor's title call.
`mock-model --listen 0.0.0.0:80 [--models models.json] [--wire openrouter|anthropic]`.

## fake-tools

The tools REST API (decision 15, the shape of jarvis3's tools): `GET /v1/tools`, `POST /v1/tools/<name>` (JSON
arguments; the text as `text/plain`; 400 bad arguments, 404 an unknown tool) and `GET /v1/openapi.json`, with `echo`
and `slow_echo` behind the tools pair tokens (one per line of the token file, for a rotation), recording calls with
`Aviary-Sandbox-Person` and `Aviary-Sandbox-Conversation` (`GET /__calls`). The token file is read again on every request, so a
rotation is driven by changing the mounted Secret (current plus `_NEXT`, then the next one alone) without a
restart; an unreadable or empty file keeps the last tokens. `fake-tools --listen 0.0.0.0:80 --token-file <path>`.

## fake-access

Cloudflare Access's JWKS, `get-identity` (cookie `CF_Authorization`; checks `exp`, `nbf` and, with `--aud`, that
the token's `aud` lists the application's audience tag, as Cloudflare's per-application identity endpoint does),
`POST /__mint` (`nbfSeconds` for a not-yet-valid token) and `POST /__keys` (publish or retire a loaded kid at run
time, for the rotation test), RS256 with ring, TLS with the harness CA. RSA keys come from the harness (`openssl genpkey`, PKCS#8 PEM): ring cannot generate RSA keys and
the `rsa` crate has an unfixed advisory (RUSTSEC-2023-0071) that `cargo audit --deny warnings` refuses.
`fake-access --team-domain <d> [--aud <tag>] --key k1=<pem> [--key k2=<pem>] [--next-key k3=<pem>] [--tls-cert <pem> --tls-key
<pem>]` (a `--next-key` is loaded but published only by `/__keys`).

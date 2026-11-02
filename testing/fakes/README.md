# Test fakes (WP-08)

In-process fakes for the other crates' tests (`aviary-fakes`) and the standalone fakes the harness runs
(`mock-model`, `fake-tools`, `fake-access`; platform spec 7.4-7.6).

## aviary-fakes

`FakeMagpie`, `FakeHermit` and `FakeSupervisor` implement the `Server` traits of `magpie-api`, `hermit-api` and
`supervisor-api` (`ControlServer` and `LocalServer`) and are served by those crates' own routers, so paths,
extractors, status codes and (for hermit) the caller-token guard are the contract's.

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
  - magpie: the sandbox surface (lease and epochs, `not_current_sandbox`, `lease_lost`; `~/.claude` files and
    directories with the `offset_beyond_eof`, missing-parent, `protected`, `exists`, `not_empty` and quota rules
    and `opId` replays; the command queue with its long-poll and acks; blobs by sha256; reports accepted). The
    person, hermit and ops routes have no model and answer 501 `not_supported` unless scripted.
  - hermit: deployment, the catalogue rules, one sandbox per conversation (idempotent `PUT`, the long-poll `GET`,
    release, the YYC 501 for pause/resume/snapshot), the feed (`latest`, order, `limit`, long-poll, 410), signed
    sandbox tokens and their keys, liveness, the descriptor with etag and 304, secrets by grant, activity reports,
    workspaces. Starting sandboxes become ready after `HermitConfig::ready_after` or on `make_ready`.
  - supervisor: the control token, health and state from a settable run state, release refused while busy unless
    forced, prepare, resumed; the local socket's `send-user-file` and `secret`.
- **Faults** (`fault.rs`), per route name or path template, for N requests or until removed: `Latency`,
  `Unavailable` (503 with `retry-after`), `Hold` (until released; `wait_held` to synchronise), `Drop` (the
  connection closes with no answer; needs `serve`, whose accept loop can drop a connection).

The fixture tests (`tests/*_fixtures.rs`) send every golden fixture of the three contract crates through the
contract clients against the fakes; `tests/faults.rs` exercises each knob; `tests/models.rs` the models.

## mock-model

The Messages API and the models route, scripted with `POST /__script` (platform 7.5). Its streams are written in
the wire form of the upstream YYC's gateway relays to (OpenRouter's Messages API), field by field, and
`tests/mock.rs` checks them byte for byte against the recordings in `testing/mock-model/recorded/`.
`mock-model --listen 0.0.0.0:80 [--models models.json]`.

## fake-tools

A Streamable-HTTP MCP server at `/mcp` with `echo` and `slow_echo` behind the tools pair token, recording calls
with `Aviary-Sandbox-Person` (`GET /__calls`). `fake-tools --listen 0.0.0.0:80 --token-file <path>`.

## fake-access

Cloudflare Access's JWKS, `get-identity` (cookie `CF_Authorization`) and `POST /__mint`, RS256 with ring, TLS with
the harness CA. RSA keys come from the harness (`openssl genpkey`, PKCS#8 PEM): ring cannot generate RSA keys and
the `rsa` crate has an unfixed advisory (RUSTSEC-2023-0071) that `cargo audit --deny warnings` refuses.
`fake-access --team-domain <d> --key k1=<pem> [--key k2=<pem>] [--tls-cert <pem> --tls-key <pem>]`.

# aviary

pigeon, magpie, hermit and the sandbox supervisor: an AI chat runtime with Claude Code in per-conversation sandboxes.

| Service | Crate | What it does |
|---|---|---|
| pigeon | `crates/pigeon` | the app-facing front end: speaks the Paseo app protocol, signs people in, streams conversations |
| magpie | `crates/magpie` | the store of record: conversations, events, turns, files and uploads (Postgres + a bucket) |
| hermit | `crates/hermit` | sandbox lifecycle behind the hermit contract; this crate is one deployment's implementation |
| supervisor | `crates/supervisor` | runs inside each sandbox: drives Claude Code, carries its transcript to magpie |

Shared crates: `aviary-core` (the service runtime), `aviary-pg` (Postgres), and the contract crates `magpie-api`,
`hermit-api`, `supervisor-api`, `gateway-api`, `sandboxd-api` (the only coupling between services). `sandboxd` is the
per-host sandbox daemon. `testing/` holds the fakes, the mock model, the reference gateway, the conformance suites and
the end-to-end harness, and `testing/paseo-contract` (Node) checks Paseo frames against every supported app build
([its README](testing/paseo-contract/README.md)). The endpoint names and conventions for implementers of another hermit or gateway are in
[docs/CONTRACTS.md](docs/CONTRACTS.md).

## Building

The toolchain is pinned in `rust-toolchain.toml`; service binaries are static musl builds linked with rust-lld
(`cargo build --release --target x86_64-unknown-linux-musl`), except magpie, which links libheif and is built on
Debian bookworm with `libheif-dev` (`.github/ci/build-magpie.sh`).

The musl build needs a musl C compiler as well as rust-lld: ring (under rustls) compiles C with cc-rs, and
`.cargo/config.toml` points `CC_x86_64_unknown_linux_musl` at `musl-gcc` (Debian/Ubuntu package `musl-tools`).
rust-lld only replaces the linker. `testing/tls-canary` keeps this path tested: CI builds, links and runs its unit
tests for musl.

The TLS stack is rustls on ring, one version each of tokio, hyper and rustls, and no OpenSSL or aws-lc (`deny.toml`
bans them). The AWS SDK crates have their default features off, so they have no HTTP client of their own. Pass
`aws_smithy_http_client::Builder::new().tls_provider(Rustls(Ring)).build_https()` as `http_client` (see
`testing/tls-canary`).

```
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo run --locked -q -p aviary-source-rules --bin tx-rule-conf -- target/tx-rule
CLIPPY_CONF_DIR=target/tx-rule cargo clippy --workspace --all-targets --locked -- -A clippy::disallowed_methods -D warnings
cargo nextest run --workspace --locked
cargo deny --locked check && cargo audit --deny warnings
```

Tests that need Postgres read `AVIARY_TEST_DATABASE_URL`. CI starts the production Postgres image, pinned to the
digest production runs, with TLS on (`.github/ci/postgres.sh`). Tests that need S3 read `AVIARY_TEST_S3_ENDPOINT`,
`AVIARY_TEST_S3_REGION`, `AVIARY_TEST_S3_ACCESS_KEY_ID` and `AVIARY_TEST_S3_SECRET_ACCESS_KEY`, use path-style
addressing and create their own buckets. CI serves S3 with versitygw, pinned by digest (`.github/ci/s3.sh`).

### Where this differs from the platform spec

- The S3 test server is versitygw, not MinIO. MinIO publishes nothing that can be pulled: docker.io/minio/minio is
  gone, quay.io/minio/minio refuses anonymous pulls, and dl.min.io answers 410 (checked 2026-11-02).
- The musl build needs `musl-gcc` for C dependencies. The spec says "no musl-gcc needed", which is true only for
  linking.
- magpie is built by a `docker run` of the pinned bookworm image plus `libheif-dev`, as the runner's uid, rather than
  in a job container, so it shares the build job's `dist/` and cargo cache.
- `cargo deny` checks platform 4.5's EC2/Kubernetes ban with `deny-hermit-only.toml`, a second config run with hermit
  and the harness crates excluded, instead of wrappers in `deny.toml`. cargo-deny warns "unmatched wrapper" for every
  wrapper that is not in the graph.

- `aviary-pg`'s transactions are a macro, `aviary_pg::tx!(&pool, |t| { … })`, not a function: a lint level can be
  set only on code the caller wrote, and a clippy `disallowed-methods` list has no scope, so "no network call inside
  the closure" takes the macro's `#[deny]` plus a second clippy pass with its own configuration (Source rules below).
  `Pool::get` returns an `aviary_pg::Conn`, not deadpool's client: it refuses `begin`, `commit`, savepoints and the
  like and has no `transaction()`, so `tx!` is the only way into a transaction.
- A `tx!` closure that ignores a statement error and returns `Ok` gets `Error::RolledBack`: the failed statement
  aborted the transaction, and Postgres answers `COMMIT` of an aborted transaction with `ROLLBACK` and no error
  (tokio-postgres does not expose the command tag). Whenever a statement failed or was cancelled, `tx!` asks the
  server (`select 1` fails with `25P02` in an aborted transaction) before it commits.
- Statements are cached per connection. A rolling release whose migration adds a column makes an old replica's
  cached `select *` / `returning *` fail with `cached plan must not change result type` (`0A000`); aviary-pg then
  clears every cached statement of the pool and retries where that is safe (outside a transaction, or as a
  transaction's first statement, which restarts it). Later in a transaction the error is returned; the next
  transaction works. Naming the columns avoids it.
- `aviary_pg::migrate` waits for the migration lock by polling `pg_try_advisory_lock` every 200 ms, not by blocking
  in `pg_advisory_lock`: a blocked statement holds a snapshot, and `create index concurrently` in the holder waits
  for every older snapshot, so blocked starters would stall it. The lock, the one-transaction-per-migration rule and
  the `_migrations` table are as platform 4.3 says (magpie and hermit spec's `schema_migrations` is platform's
  `_migrations`). A migration whose first line is `-- aviary-pg: no-transaction` runs outside a transaction and holds
  exactly one statement (for `create index concurrently … if not exists`; comments, strings and dollar bodies are
  lexed when counting).
- The spec says nothing of lock waits during migrations. Each transactional migration runs with `lock_timeout = 5s`
  and is retried up to 6 times (pauses 1, 2, 4, 8, 16 s): an `alter table` queued behind a long transaction would
  otherwise hold every later query on the table behind its `ACCESS EXCLUSIVE` request and stall the running
  replicas. A no-transaction migration runs without the limit (`create index concurrently` blocks no reads or
  writes and waits for older transactions by design).
- A failed or killed `create index concurrently` leaves an invalid index that `if not exists` would accept for good.
  Before each no-transaction migration `migrate` drops every invalid index of the database that is not being built
  right now, and after it fails the start (`Error::InvalidIndex`, the index dropped) if one is left.
- `aviary_pg::Leader::start_many(pool, &[keys], retry)` holds several keys on one dedicated connection, so hermit's
  janitor (7300,3) and scaler (7300,4) use the one leader connection platform 11.4 budgets per pod. The holder
  checks its connection every `retry` in the background; callers run `Leader::confirm()` (`select 1` on the leader
  connection, 2 s timeout) before every pass, as hermit 10.3 says, so a half-open connection stops the loop at once.

## The service runtime (`aviary-core`)

Every service binary starts the same way: `version::Build` (name and protocol surfaces), `log::init` (JSON lines,
every string field redacted), `auth::Callers::from_env` (`<CALLEE>_ACCEPT_<CALLER>`), `ops::Ops` on `:9090`
(`/readyz` 503 until `Readiness::started()`, with named gates for the parts still pending), the API router wrapped by
`http::standard` and guarded per router by `auth::Guard::router` (a guard covers only the routes it was given), then
`drain::Drain::run` on SIGTERM. The crate docs (`crates/aviary-core/src/lib.rs`) show the outline.

Where it differs from the platform spec (sections 3, 4.3, 15.1):

- Test code reads `AVIARY_TEST_*` variables with `env::test_only`, which refuses any other name and exists only
  behind `aviary-core`'s `testing` feature; the source rules refuse that feature outside `[dev-dependencies]` of the
  crates under `crates/`, so services still have no optional configuration. The source rules allow `std::env::var*` only in
  `aviary-core/src/env.rs`, and clippy's ban applies to tests too.
- Pair tokens are compared with `subtle::ConstantTimeEq` on the SHA-256 digests. ring 0.17 deprecates
  `constant_time::verify_slices_are_equal` (named by the spec) as an internal function "with no promises regarding
  side channels".
- `log::redact` runs the hermit-token rule (`hst1.`, `hct_`) before the long-run rule, so short tokens are caught too.
  Inside a long run, a UUID or person key is kept and the pieces around it are judged on their own length, so ids in
  URLs stay searchable. Beyond 15.1's list it also turns emails (`a@b.tld`, `a%40b.tld`) into their `person_key`
  (3.8: logs never carry emails), keeps jarvis3's parameter rule whole (`=` or `:`, optional quotes, the names
  `passwd` and `authorization` too, so JSON bodies, `Debug` structs and header dumps are covered; `person_key` is
  exempt), and adds `cookie` (15.1: cookies are never logged; a `cookie`/`set-cookie` value goes to the end of the
  line). That rule skips Debug wrappers (`password: Some("…")`), redacts wrappers named like a secret
  (`Secret("…")`), takes a quoted value whole up to its closing quote, and redacts the word after `Bearer`/`Basic`
  whatever its shape when a credential name comes before it. Without such a name, only a token-shaped word after
  `Bearer`/`Basic` is redacted (so `Basic info` in a message stays). URL userinfo runs up to the last `@` of the
  authority, or, for a `user:` password holding a raw `/`, `?` or `#`, up to the URL's last `@`. `%XX` escapes count
  as part of a long run.
- `/readyz` starts at 503 with `why: ["start"]` and is 200 only after `Readiness::started()`, so a process is never
  ready before its own start is done, even before its first named gate exists.
- A token configured for two different callers fails start (`CallersError::SharedToken`) with the start-failure
  line (`msg` `invalid env`, the variable's name and which other variable shares its token) and exit 78.
- `env::required` returns the value trimmed, as the callee side compares tokens, so a Secret written with a trailing
  newline works on both sides.
- debug and trace events are compiled out of release builds (tracing's `release_max_level_info`); test and dev
  builds keep them, and the runtime filter still writes only `info` and above.
- `/metrics` is labelled 0.0.4 as 3.2 says, so prometheus-client's OpenMetrics output is rewritten: a counter's
  `# HELP`/`# TYPE` lines take the sample's `_total` name (a 0.0.4 parser types only a sample named exactly as its
  `# TYPE` line), and `# UNIT`, `# EOF`, counters' `_created` samples and exemplars are dropped.
- The outbound client has a 120 s read timeout (`http::DEFAULT_READ_TIMEOUT`, a stalled callee cannot hang a call
  whose site forgot its own `.timeout()`); a client whose callee is silent longer sets its own on the builder. Under
  `http::retry::send` a call's timeout applies per attempt; `Backoff::worst_case` gives the total bound.
- `Aviary-Request` is kept only when it is a v7 UUID; any other value, a v4 UUID included, is replaced by a minted v7.
- Error answers that are not JSON (axum's extractor rejections, unmatched routes, wrong methods, a body over the
  limit) are rewritten into the envelope by `http::envelope`; axum's 422 for an unusable JSON body becomes 400
  `bad_request`, since the platform's code list has no 422. The rewrite uses 3.6's codes, plus two that 3.6 does not
  list and hermit's contract also uses: `method_not_allowed` (405) and `internal` (500 and any other 5xx). 403
  becomes `wrong_caller` and 410 `cursor_expired` (3.6's codes for them), 408/502/504 `unavailable`, and every other
  4xx (409, 411, 412, 416, 431…) `bad_request`, so a client error is never reported as a server fault.
  `http::standard` is only for the `/v1` service APIs: pigeon's public paths (the frozen app protocol and the public
  site) keep their own error bodies and use `http::request_id` and `http::defaults` alone.
- The missing-env line carries `t`, `level`, `svc`, `ver` and (once known) `pod` before `msg`; `svc` is the name
  given to `log::init`/`log::init_in_pod`, or the binary's name when the variable is read before that.
- `/version` lists every surface in `accepts`, including a surface at number 1 (`[1]`); the spec's example omits it.
- The start-failure line goes where `log::init` sends logs (stderr for the supervisor), stdout before that.
- Exit codes, which the spec leaves open: 78 (`EX_CONFIG`) for a missing, empty or invalid variable, 0 after a drain, 143
  when a second signal cut the drain short.

## Source rules

CI enforces these over the source tree (`testing/source-rules`, `clippy.toml`, `deny.toml`):

- configuration is read only through `aviary_core::env` (no `std::env::var*` anywhere else);
- no crate under `crates/` except `hermit` depends on `aws-sdk-ec2` or `kube`;
- `crates/pigeon`, `crates/magpie`, `crates/supervisor` and `crates/*-api` name no particular deployment or provider,
  and pigeon's frozen app wire constants live only in `crates/pigeon/src/paseo/compat.rs`;
- every dependency's licence is compatible with AGPL-3.0-or-later; advisories fail the build;
- no transaction is held across a network call: a network call inside the closure of `aviary_pg::tx!` is a clippy
  error. The rule is a second clippy pass whose configuration `aviary_source_rules::tx_rule` generates from the tree
  (`--bin tx-rule-conf`): the repository's `clippy.toml` plus `reqwest`'s calls, every `pub async fn` of the
  contract crates' typed clients (`crates/*-api/src/client.rs`), `aviary_core::http::send`, `aws-config`'s loaders,
  and every operation of the AWS SDKs the workspace depends on (S3 now; EC2 once hermit depends on it), with
  `-A clippy::disallowed_methods`; the macro marks its closure `#[deny(clippy::disallowed_methods)]`, so the lint
  fires there only. A call made in a helper function that the closure calls is not seen (the lint level belongs to
  the helper's own code), nor is a client the generator does not know. The first pass's `clippy.toml` disallows the
  other ways into a transaction: `tokio-postgres`' and deadpool's `transaction()`/`build_transaction()`, and calling
  `aviary_pg::__run_tx` by hand.

The repository is public: it holds no keys, tokens or deployment configuration. Test credentials are generated at run
time.

## Licence

AGPL-3.0-or-later (see [LICENSE](LICENSE)). pigeon includes code derived from CloudCLI (AGPL-3.0).

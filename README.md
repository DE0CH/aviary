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
the end-to-end harness. The endpoint names and conventions for implementers of another hermit or gateway are in
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

## Source rules

CI enforces these over the source tree (`testing/source-rules`, `clippy.toml`, `deny.toml`):

- configuration is read only through `aviary_core::env` (no `std::env::var*` anywhere else);
- no crate under `crates/` except `hermit` depends on `aws-sdk-ec2` or `kube`;
- `crates/pigeon`, `crates/magpie`, `crates/supervisor` and `crates/*-api` name no particular deployment or provider,
  and pigeon's frozen app wire constants live only in `crates/pigeon/src/paseo/compat.rs`;
- every dependency's licence is compatible with AGPL-3.0-or-later; advisories fail the build.

The repository is public: it holds no keys, tokens or deployment configuration. Test credentials are generated at run
time.

## Licence

AGPL-3.0-or-later (see [LICENSE](LICENSE)). pigeon includes code derived from CloudCLI (AGPL-3.0).

# sandboxd

The agent on every sandbox host of hermit's fabric (and on its WireGuard gateway): one static musl binary with a
fixed, provable memory bound. It creates gVisor sandboxes from unpacked images, gives each its network namespace,
address and nft lock, keeps the host's WireGuard peers and the gateway's egress translation, serves images to the
other hosts, and live-migrates sandboxes between hosts (memory pre-copy with the DE0CH/gvisor fork, plus the scratch
filesystem by rsync). hermit is its only client.

- API: crate [`sandboxd-api`](../sandboxd-api) (types, client, server trait, golden fixtures). sandboxd writes and
  parses the same JSON by hand; `src/contract.rs` and `crates/sandboxd-api/tests/live.rs` check both against the
  fixtures and the running daemon.
- Memory bound and how it is enforced and tested: [docs/MEMORY.md](docs/MEMORY.md).
- Network, storage, migration, security on a host: [docs/HOST.md](docs/HOST.md).
- Host scripts: `fabric/host` (setup.sh, gateway.sh, make-certs.sh, fetch-sandboxd.sh), `fabric/aws` (infra.sh and the
  IAM policies), `fabric/gvisor-patches`; the gateway installer image `images/sandboxd`.

Moved from DE0CH/jarvis3 `chat/sandboxd/daemon` on 2026-11-02 (jarvis3 commit 52c9b8c, platform spec 4.1); jarvis3
keeps its own copy for the old fabric until the switch's P5.

## Build and test

```
cargo build --profile sandboxd --locked --target x86_64-unknown-linux-musl -p sandboxd   # static, unwinds on panic
cargo test -p sandboxd -p sandboxd-api
sudo -E env PATH="$PATH" python3 crates/sandboxd/tests/soak.py \
  target/x86_64-unknown-linux-musl/sandboxd/sandboxd --seconds 45 --dump /tmp/sbx-dump
SANDBOXD_DUMP=/tmp/sbx-dump cargo test -p sandboxd-api --test live -- --ignored
```

The release build must use profile `sandboxd` (`panic = "unwind"`: each connection runs in `catch_unwind`); the
workspace release profile aborts on panic. CI (`ci.yml`): `test` runs the unit tests, `sandboxd-soak` the soak and
the live contract check, `build` puts the binary in `dist` (checked static, `--version`, `budget`).

## Commands

- `sandboxd` serves, configured by `/etc/sbx/host.json` (`SBX_CONFIG` to point elsewhere).
- `sandboxd budget [host.json]` prints the memory budget for a configuration.
- `sandboxd migrate-relay …` is the target's end of a migration's scratch copy (started by sandboxd itself).
- `sandboxd --version`.

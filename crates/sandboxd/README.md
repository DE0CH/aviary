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

## Behaviour changed in the move

One change, deliberately, and recorded as a deviation from "behaviour unchanged" (WP-06): the client certificate CN
sandboxd serves is `hermit`, not jarvis3's `sandbox-controller` (`src/server.rs` `CONTROLLER_CN` =
`sandboxd_api::CLIENT_CN`). It is coupled to `fabric/host/make-certs.sh`, which issues the client certificate as
`hermit` (hermit spec 13.2 item 8, 14.5); keeping the old CN would have meant a make-certs.sh that issues a name
hermit's spec does not use. So a sandboxd built from aviary refuses jarvis3's controller certificate (403). That is
harmless while the old fabric runs jarvis3's own copy of sandboxd (until P5), and nothing on the old fabric installs
aviary's binary. Everything else on the wire is unchanged.

## hermit spec 13.2's changes (WP-19)

All done (docs/HOST.md, "sandboxd"): the first process from the image's `Entrypoint` + `Cmd` (item 1; 400 without an
entrypoint), no `~/.claude` bind and no `user` (item 2; an unknown spec field is a 400), no quota and no folders made
or chowned on JuiceFS (item 3; 503 without the mount, 409 `workspace missing`), no `/etc/hosts` (item 4; there was
none in this version), `alive` from `cgroup.events` (item 6), `/dev/fuse` in the bundle (item 7; no runsc flag: the
fork registers FUSE unconditionally, `runsc/boot/vfs.go`, and the FUSE gate ran with these flags), the CN `hermit`
(item 8), pids from the spec (item 9; `pids` in [64, 1048576], absent = 4096, written back only when given),
`GET …/log?tail=` (item 10) and the receive's state without a wait (item 11; `{"state": "none"}` for a name with no
receive record). Items 5 (network) and everything else are unchanged.

## Deployment defaults still compiled in

sandboxd was moved with its behaviour unchanged, so it still has these defaults of the old jarvis3 deployment (their
values are in the source, not repeated here). Every deployment sets the fields in host.json or the spec; hermit spec
13.2 (WP-19) makes them required or neutral and removes the old values:

| Default | Where | Value |
|---|---|---|
| `controlCidr` | `src/config.rs` | the old Hetzner private network |
| `hostCidr`, `sandboxCidr`, `egressVip` | `src/config.rs` | the old fabric's ranges |
| `privateGateway` when empty | `src/net.rs` (gateway and hub branches) | the old Hetzner network's gateway |
| `podCidr` when empty | `src/net.rs` | k3s's default pod range |
| the image base of a pull without a reference (`ref`, `imageRef`) | `src/images.rs` | jarvis3's sandbox image on ghcr.io (wrong once hermit's sandbox image replaces it) |
| `imageSource` when empty (where a burst host fetches an image by digest) | `src/images.rs` | `http://` + the old fabric's hub address + `:<imagePort>` (on hermit's fabric the gateway's overlay address, so a host.json without it fetches from nothing) |

For WP-19: `imageSource` becomes required for a burst host (hermit's scaler writes the gateway's overlay address,
`http://<gateway wgAddress>:<imagePort>`, into every user-data host.json; docs/HOST.md, "host.json"). The unit tests in
`src/` and `tests/soak.py` (with `tests/fake/`) use the old fabric's addresses because they exercise these defaults;
they move to placeholder ranges with them.

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
the live contract check, `build` puts the binary in `dist` (checked static, `--version`, `budget`) and builds and runs
the gateway installer image `images/sandboxd` (not pushed).

No `aviary-<sha12>` release is published yet: that is the `publish` job of WP-28 (main only, after every gate). Until
it exists, `fabric/host/fetch-sandboxd.sh` (and so `setup.sh`, `gateway.sh` and `infra.sh ami`) installs sandboxd
only from a local build or the `dist` artifact, with `SANDBOXD_BINARY=<path>`. `infra.sh sync` refuses both a local
build and a `fabric.sandboxd` pin that is not an `aviary-<sha12>` release (exit 3, nothing baked), and `check` says
why it fails, so the AMI workflow (hermit spec 14.3) cannot bake an AMI on every run before a release exists.

## Commands

- `sandboxd` serves, configured by `/etc/sbx/host.json` (`SBX_CONFIG` to point elsewhere).
- `sandboxd budget [host.json]` prints the memory budget for a configuration.
- `sandboxd migrate-relay …` is the target's end of a migration's scratch copy (started by sandboxd itself).
- `sandboxd --version`.

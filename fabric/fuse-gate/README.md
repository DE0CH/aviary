# FUSE gate, host side (WP-07)

Re-runs the gate recorded in `crates/supervisor/GATE.md` on throwaway Ubuntu 24.04 hosts (root; two of them for a
migration, reachable from each other on TCP 22 and 7000). The in-sandbox side is `crates/supervisor/spikes/fuse-gate`
(build it with `cargo build --release --target x86_64-unknown-linux-musl` there and copy `fuse-gate` plus `sandbox/*`
to `/var/lib/gate/bin/` on both hosts with identical bytes and mtimes, e.g. `rsync -ac`: a migration restores only
onto identical host files). `LAYOUT=scratch` migrations also need `sandboxd` (this repo's `crates/sandboxd`, musl
build) at `/usr/local/bin/sandboxd` on the target for its `migrate-relay`, and `rsync` on both hosts.

```bash
./host-setup.sh ghcr.io/de0ch/jarvis3-sandbox:<tag>   # every host: runsc release, crane, the image, ckpt tmpfs
./mount-check.sh m1                 # items 1 and 5 (locks.py)
./session.sh s1 fuse                # item 2 (and 9 with STRACE=1); ./session.sh s1 local = the tmpfs reference
MOUNT=/tmp/lh/.claude python3 strace-inventory.py /var/log/gate/strace-s1/*boot* > inventory.json   # (local run)
./tombstone-try.sh t1 thinkfail     # item 2: Claude Code's own tombstone triggers (also: refusal, catchall, catalog, overload)
./killtest.sh k1 [/mnt/fuse-journal]   # item 4 and the keeper-only hold
./sigresend.sh g1                   # does a signal (not a checkpoint) resend a held request?
./migrate.sh m1 <target ip> [64 87] # item 3 (target reachable as root with /root/.ssh/gate), full cache with 64 87
LAYOUT=scratch ./migrate.sh m2 <target ip>   # the same, cache/journal on scratch moved by rsync + sandboxd's relay
./migrate-batch.sh <target ip> e 20 [tmpfs|scratch [64 87]]   # N runs, every run's outputs kept
HOLDS='1 2' ./migrate-batch.sh <target ip> h 24 scratch   # alternating with the matched control (HOLD=2)
./latency.sh l1                     # items 6 and 10
./resume87.sh r1                    # item 6: Claude Code resumes an 87 MiB transcript from a cold cache
python3 uniques.py ops.tsv [boundary_us ...]   # item 4: unique never repeats (used by migrate.sh, killtest.sh)
```

Results land in `/root/out/`. Delete the hosts afterwards. IPv6-only hosts work (the rsync URL is bracketed, the
relay listens on `[::]`; `GATE_RELAY_LISTEN` overrides).

## The re-run on the real build (WP-22, `rig/`)

The same hosts and `gate.sh`, with the real `supervisor` and `supervisor-keeper` (musl release builds of
`crates/supervisor`; `supervisor-fi` built with `--features fault-injection`) and `fuse-rig` (`testing/fuse-rig`: fake
hermit and fake magpie behind the reference gateway, inside the sandbox on loopback, with an admin port on
`127.0.0.1:7300`) in `/var/lib/gate/bin/rig/` next to `rig/{init.sh,rigop.py,rigctl.py,locks.py}`, identical on both
hosts. `gate.sh bundle` with `RIG=1` makes `rig/init.sh` the first process: it starts the world, then execs the
keeper as PID 1, which starts `/usr/local/bin/supervisor run` (the rig's binary bound over the image's path).

```bash
./rig/host-rig.sh                          # once per host: Claude Code at /opt/claude-code/bin/claude, the stdout tap
./rig/rig-mount.sh m1                      # items 1 and 5 on the real build
./rig/rig-kill.sh k1                       # item 4: the fault-injection build SIGKILLs itself at each journal point
./rig/rig-mig.sh w1 write <target ip>      # one migration with a write (or create, mkdir, rename) held in magpie
./rig/rig-batch.sh X 10 <target ip>        # 10 rounds of the four ops in both layouts (LAYOUTS, OPS)
LOOPBACK=1 ./rig/rig-batch.sh L 10         # the same with the target on this host (gate.sh GATE_SIDE=b)
./rig/rig-session.sh s1                    # item 2: Claude Code launched by the real supervisor (6.1-6.4), turns as
STRACE=1 ./rig/rig-session.sh s2           #   magpie start_turns; item 9 (then strace-inventory.py on the boot log)
./rig/rig-tomb.sh t1 [catalog|catalog2]    # item 2's tombstone triggers under the supervisor's argv
./rig/rig-latency.sh l1                    # items 6 and 10 (bench, message latency, 87 MiB cold resume)
LOOPBACK=1 [FILL=1] ./rig/rig-migcc.sh m1  # item 3 with Claude Code mid-turn and every cc write held (FILL: full cache)
```

For the Claude Code items fuse-rig's world also runs the gate's mock model (`/var/lib/gate/bin/fuse-gate`, started by
`rig/init.sh`) and fake-tools behind the gateway; `rig/rigsession.py` drives turns through the world's admin port and
`rig/mksessions.py` writes the scripted sessions. `rig-migcc.sh` needs `sandboxd` (its `migrate-relay`) at
`/usr/local/bin/sandboxd` and `/root/.ssh/gate` accepted by the target (the host itself for `LOOPBACK=1`).

Results land in `/root/out/{rig,kill,rmount}-NAME/`. A target with no public address works over a private network
(`rig-mig.sh` reaches it by SSH as root with `/root/.ssh/gate` and sends the stream to `[ip]:7000`).

# FUSE gate, host side (WP-07)

Re-runs the gate recorded in `crates/supervisor/GATE.md` on throwaway Ubuntu 24.04 hosts (root; two of them for a
migration, reachable from each other on TCP 22 and 7000). The in-sandbox side is `crates/supervisor/spikes/fuse-gate`
(build it with `cargo build --release --target x86_64-unknown-linux-musl` there and copy `fuse-gate` plus `sandbox/*`
to `/var/lib/gate/bin/` on both hosts with identical bytes and mtimes, e.g. `rsync -ac`: a migration restores only
onto identical host files). `LAYOUT=scratch` migrations also need `sandboxd` (this repo's `crates/sandboxd`, musl
build) at `/usr/local/bin/sandboxd` on the target for its `migrate-relay`, and `rsync` on both hosts.

```bash
./host-setup.sh ghcr.io/de0ch/jarvis3-sandbox:<tag>   # every host: runsc release, crane, the image, ckpt tmpfs
./mount-check.sh m1                 # item 1
./session.sh s1 fuse                # item 2 (and 9 with STRACE=1); ./session.sh s1 local = the tmpfs reference
MOUNT=/tmp/lh/.claude python3 strace-inventory.py /var/log/gate/strace-s1/*boot* > inventory.json   # (local run)
./tombstone-try.sh t1 thinkfail     # item 2: Claude Code's own tombstone triggers (also: refusal)
./killtest.sh k1 [/mnt/fuse-journal]   # item 4 and the keeper-only hold
./sigresend.sh g1                   # does a signal (not a checkpoint) resend a held request?
./migrate.sh m1 <target ip> [64 87] # item 3 (target reachable as root with /root/.ssh/gate), full cache with 64 87
LAYOUT=scratch ./migrate.sh m2 <target ip>   # the same, cache/journal on scratch moved by rsync + sandboxd's relay
./migrate-batch.sh <target ip> e 20 [tmpfs|scratch [64 87]]   # N runs, every run's outputs kept
./latency.sh l1                     # items 6 and 10
./resume87.sh r1                    # item 6: Claude Code resumes an 87 MiB transcript from a cold cache
python3 uniques.py ops.tsv [boundary_us ...]   # item 4: unique never repeats (used by migrate.sh, killtest.sh)
```

Results land in `/root/out/`. Delete the hosts afterwards.

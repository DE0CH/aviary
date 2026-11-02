# FUSE gate, host side (WP-07)

Re-runs the gate recorded in `crates/supervisor/GATE.md` on two throwaway Ubuntu 24.04 hosts (root, IPv6
between them). The in-sandbox side is `crates/supervisor/spikes/fuse-gate` (build it with `cargo build --release
--target x86_64-unknown-linux-musl` there and copy `fuse-gate` plus `sandbox/*` to `/var/lib/gate/bin/` on both
hosts with identical bytes and mtimes, e.g. `rsync -ac`: a migration restores only onto identical host files).

```bash
./host-setup.sh ghcr.io/de0ch/jarvis3-sandbox:<tag>   # both hosts: runsc release, crane, the image, ckpt tmpfs
./session.sh s1 fuse          # items 2 (and 9 with STRACE=1); ./session.sh s1 local = the tmpfs reference
python3 strace-inventory.py /var/log/gate/strace-s1/*boot* > inventory.json
./killtest.sh k1 [/mnt/fuse-journal]   # item 4 (and the keeper-only hold)
./migrate.sh m1 <target ipv6> [64 87]  # item 3 (target reachable as root with /root/.ssh/gate), full cache with 64 87
./latency.sh l1               # items 6 and 10
```

Results land in `/root/out/<test>-<name>/`. Delete the hosts afterwards.

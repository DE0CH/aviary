# WP-57: the two faults after a restore (2026-11-02)

Raw outputs of every WP-57 run (failed ones included), on two throwaway Hetzner Cloud cx33 in fsn1 (`runsc
cpu-features` identical to round 3's hosts, IPv6 only, Ubuntu 24.04, kernel 6.8.0-138-generic), deleted after the
run. Host addresses are replaced by `<host-ipv6>`; files over 32 KiB are gzipped. GATE.md ("WP-57") quotes them.

- `stale-batch-so.log`, `stale-batch-sn.log`, `stale/stale-so*`, `stale/stale-sn*`: `fabric/fuse-gate/stale-batch.sh`
  (`stale-mig.sh` N times, `sandbox/stale.py 32` plus `stale.py --dirty 128`, tmpfs layout) on the old release
  `precopy-20261021.2` (`so`, 20 runs) and on `precopy-20261102.1` (`sn`, 40 runs). Each `stale.jsonl` is the
  program's own log on the target: a `fault` line is memory that read back with bytes it must not have (a fresh
  mapping that is not all zero, a page it overwrote with zeroes that holds older bytes). `stale/stale-o9*` are the
  four trial runs before the batch (an earlier `stale.py` that re-read a page instead of keeping the first read: o93's
  `pages: 0` line is that re-read). During `so` and `sn` two sandboxes left over from killed trial runs (each
  rewriting 300 MB in a loop) were still running on the source host; they were removed before the gate batches.
- `mmap-rsync.txt`: `fabric/fuse-gate/mmap-rsync.sh` on a gate host (ext4, rsync 3.2.7): a second write through a
  `MAP_SHARED` mapping to a page that is already dirty leaves the file's mtime unchanged, and rsync's quick check
  (size and mtime, the check of `gate.sh send-scratch` and of sandboxd's `sync_scratch`) then transfers nothing:
  the copy keeps `len` 0 where the source has 56. With a writeback (fsync) between the writes the second write
  updates the mtime and the file is copied.
- `mig-t01/`: one scratch-layout hold-on run with the checksummed final delta (`gate.sh send-scratch` now);
  `mig-q01`…`mig-q12/`, `batch-q.log`, `run-q*.txt`: 12 hold-on runs with `FINAL_QUICK=1` (sandboxd's quick-check
  final delta) and the new source/destination comparison (`files_differing_after_final` in `send.json`). `mig-q04`
  failed before any page reached the target: `runsc checkpoint --migrate-to` ended without output and the
  destination saw an incomplete stream (`runsc-target.log`); nothing in the kernel log (no OOM).

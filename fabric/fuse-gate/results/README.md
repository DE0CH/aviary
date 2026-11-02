# FUSE gate results (WP-07, 2026-11-02)

Raw outputs of every run that `crates/supervisor/GATE.md` quotes (kept here, outside the crate, because the
source rules keep deployment and runtime names out of `crates/`; the outputs contain them), failed runs included. Files over 32 KiB are
gzipped (`uniques.py` and `migsummary.py` read the unzipped files: `zcat` them first); host addresses are replaced by
`<host-ip>`, `<public-ip>` and `<host-ipv6>`.

- `aws/` — two throwaway `m7i.xlarge`, eu-central-1a (items 1, 3, 4; items 2 and 6 repeated):
  - `mount-m1/item1.txt` (item 1);
  - `mig-*/` one directory per migration (`fabric/fuse-gate/migrate.sh`): `times.json`, `receive.json` (runsc
    restore's figures), `send.json` (scratch layout: rsync rounds and the stopped delta), `drive.json` +
    `drive-stdout.jsonl` + `drive-stderr.log` (Claude Code's turns), `blocked.json` (the held 3 × 1 MiB append),
    `probe.json` (stat/read waits under the hold), `report.json` (every file vs the fake magpie's copy), `calls.log`
    (magpie's calls), `ops.tsv` (every FUSE request: start µs, opcode, node, path, detail, errno, µs, unique),
    `uniques.json`, `supervisor.log`, `exits.log` (in-sandbox process exits), `fakes.log`, `jobs.out`,
    `runsc-source.log`, `runsc-target.log`, `runsc-restore.out`, `relay.log` (scratch), `debug-faults.txt` (the
    target's sentry debug log, fault lines; batch `d` only), `failures.txt` (why the run failed; empty = pass);
    `batch-*.log` and `run-*.txt` are the batch logs. Batches: `e` tmpfs layout, `s` scratch layout, `f`/`g` the
    same with a full cache, `d` tmpfs with the target's debug log, `n` no hold (control), `l` no hold and Claude
    Code's HOME off the mount (control), `p` tmpfs with `--systrap-disable-syscall-patching` (experiment), `s0` the
    first scratch run; `migrations.json` is `fabric/fuse-gate/migsummary.py`'s table of all of them;
  - `co-*/` the copy-out control (`fabric/fuse-gate/copyout-mig.sh`);
  - `kill-k1/` (journal on tmpfs), `kill-k2/` (journal on the host-backed scratch bind), `k1.txt`/`k2.txt` (item 4
    and the keeper-only test, `keeper.txt`);
  - `st-aws/` (item 2), `r87-aws/` (item 6, the 87 MiB cold resume), `sig-aws/` (signal vs checkpoint resend).
- `hetzner/` — throwaway Hetzner cx33 and cpx32 runners (items 2, 9, 10, 6):
  - `st2/` (item 2: the scripted session, both torn tails, relocation, compaction, file history; `tmp/out-*` are
    Claude Code's stream-json, `.claude/projects` the final transcripts, `srv/` the supervisor's and fakes' logs);
  - `inv/inventory-mount.json`, `invl/inventory-local.json` (item 9; the same session under `runsc --strace`, on the
    mount and with HOME on tmpfs);
  - `tomb-t-thinkfail/`, `tomb-t-refusal/` (item 2's tombstone triggers); `sig-a/`; `r87-a/`;
  - `lat-l2/` (items 6 and 10, including `order-hold*.json`: when each held transcript append landed against each
    turn's result).
- `hetzner3/` — round 3 after the review, two throwaway Hetzner cx33 in nbg1 (AMD EPYC-Rome, IPv6 only):
  - `mount-m1/` (items 1 and 5: `item1.txt`, `locks.txt` with every opcode the server received);
  - `mig-h01`…`mig-h24/` (`batch-h.log`, `run-h*.txt`): scratch layout, alternating hold-on (odd) and the matched
    control `HOLD=2` (even); `mig-t09/` one more hold-on run; `migrations.json` their table. `ops.tsv` here also has
    ` pid=` (the calling thread) in the detail of mutating ops, `REPLYERR` lines (a refused reply write) and would
    have `SHORTREAD` lines (none occurred); `uniques.json` lists each resend;
  - `tomb-t1`…`tomb-t5/` (item 2's tombstone triggers: `catchall` once and refused twice, `overload`, `catalog` once
    and refused twice);
  - `setup-trials/` (`mig-t01`…`t08`): runs that failed on the scripts' own IPv6 bugs (the rsync URL, the relay's
    IPv4-only listen address) and on a leftover relay from a killed run, before any migration happened; kept because
    every run is kept.
- `aws-recheck.txt` — every committed migration re-checked with round 3's fail rules (Claude Code's exit, supervisor
  faults, `exits.log`): `mig-s09`, counted as passed, fails on two supervisor faults (GATE.md deviation 8).
- `run1-hetzner/` — the gate's first run (Hetzner cx33, hel1 → fsn1). Superseded by the above; kept as it was. Its
  outputs are incomplete (the hosts were deleted before every run's files were saved): only the files present here
  exist.

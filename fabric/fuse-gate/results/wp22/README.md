# WP-22: the gate re-run on the real build (2026-11-02)

Raw outputs of every run (none failed) of `fabric/fuse-gate/rig/` with the real `supervisor` and `supervisor-keeper`
(musl release builds of `crates/supervisor` at 61023e3; `supervisor-fi` the same with `--features fault-injection`)
and `fuse-rig` (`testing/fuse-rig`), on two throwaway Hetzner Cloud ccx23 in fsn1 (AMD EPYC-Milan, Ubuntu 24.04,
kernel 6.8.0-138-generic, runsc `precopy-20261102.1`, image `ghcr.io/de0ch/jarvis3-sandbox:2eb5bf396b5f`), deleted
after the run. The source host had a public IPv6 address; the target had none (the account's primary-IP limit) and
was reached over a Hetzner private network. Host addresses are replaced by `<host-ipv6>` and `<private-ip>`; files
over 32 KiB are gzipped. `crates/supervisor/GATE.md` ("WP-22") quotes them.

- `rig-batch-X.log`, `rig-X*/`: 80 migrations from the source host to the target (10 rounds of `write`, `create`,
  `mkdir`, `rename` in the `tmpfs` (`-t-`) and `scratch` (`-s-`) layouts), one operation held in fake magpie across
  each. Per run (`rig-mig.sh`): `op.json` (the caller's syscall and its result), `calls.json` (every magpie call of the
  held route, with its `opId`), `opids.txt` (the distinct `opId`s after the setup), `nodes.txt` (magpie's copy of each
  path), `resend.txt` (the supervisor's resend log lines), `supervisor.log` (the keeper's and supervisor's console),
  `held-before.txt`/`held-after-move.txt`, `send.json` (scratch: rsync rounds, the checksummed final delta and the
  source/destination comparison), `receive.json`, `runsc-*.log`, `times.json`, `failures.txt` (empty = pass).
- `rig-batch-L.log`, `rig-L*/`: the same 80 with the target on the source host (`LOOPBACK=1`, `gate.sh GATE_SIDE=b`),
  run first while the second host was not yet available.
- `kill-k1/` (journal on the scratch bind) and `kill-k2/` (tmpfs): `rig-kill.sh`, the fault-injection build killed
  at each of the four journal points of a WRITE, MKDIR, CREATE and RENAME: `cases.jsonl` (one line per case),
  `calls-N.json`, `supervisor.log`.
- `rmount-m1/`: `rig-mount.sh` (items 1 and 5): `item1.txt`, `locks.txt`, `supervisor.log`.

## The Claude Code items (2, 3 with Claude Code mid-turn, 6, 9, 10)

On one throwaway AWS `m7i.xlarge` in eu-central-1 (Intel Xeon Platinum 8488C, Ubuntu 24.04, kernel 7.0.0-1014-aws,
runsc `precopy-20261102.1`, image `ghcr.io/de0ch/jarvis3-sandbox:2eb5bf396b5f`; `supervisor` and `supervisor-keeper` musl release builds of
0a06093, fuse-rig and the mock model as in this commit), deleted after the run. Host addresses are replaced by `<host-ip>`.

- `rsess-s1/`: `rig-session.sh` (item 2): `{a,b,b2}.summary.json` (each turn's end report), `report.json` (every file
  against magpie's copy, the truncates and renames), `calls-*.json` (every magpie call of those routes),
  `torn-b*.json`, `claude-process.txt` (Claude Code's argv, uid, cwd and environment, the proxy token replaced),
  `managed.txt`, `cc-stdout.jsonl` (Claude Code's stdout with arrival times), `model.log`, `supervisor.log`, the
  transcripts under `.claude/projects/`.
- `rsess-s2/`: the same session under `runsc --strace` (item 9): `inventory-mount.json` (`strace-inventory.py`).
- `rtomb-t1/` (`catalog`), `rtomb-t2/` (`catalog2`, the refusals after a written tool call): `rig-tomb.sh`.
- `rlat-l4/`, `rlat-l5/`: `rig-latency.sh` (items 6 and 10): `bench-*.json`, `c-*.summary.json`, `msgs-*.json`,
  `r87-*.summary.json`, `grow.json`, `calls-cc_file.json` (l5: what the cold resume fetched).
- `rmig-batch.log`, `rmig-e4/`, `rmig-m1/`…`m5/` (empty cache), `rmig-f1/`…`f3/` (`FILL=1`): `rig-migcc.sh` (item 3
  with Claude Code mid-turn): `turn.summary.json`, `op.json`, `w.node.json`, `report.json`, `send.json`,
  `receive.json`, `failures.txt` (empty = pass), `supervisor.log`, `cc-stdout.jsonl`.

## WP-22 review 2 (2026-11-03): item 4 on `106166f3`

One throwaway Hetzner Cloud ccx23 in fsn1 (Ubuntu 24.04, runsc `precopy-20261102.1`, image
`ghcr.io/de0ch/jarvis3-sandbox:2eb5bf396b5f`; `supervisor`, `supervisor-fi`, `supervisor-keeper` and `fuse-rig` musl
release builds of `106166f3`), deleted after the run.

- `kill-k3/`: `rig-kill.sh` (scratch layout): the four journal points of WRITE, MKDIR, CREATE and RENAME, read-returned
  and reply-written of a READ, and a plain SIGKILL during a held 1 MiB write (case 19; its hold was released by hand
  in this run, see `kill-k4`). `cases.jsonl`, `calls-N.json`, `calls-final-<route>.json`, `supervisor.log` (every
  request the test build read, as `fuse request`), `uniques.json` (`rig/rig-uniques.py`), `run.log`.
- `kill-k4/`: `OPS=plain rig-kill.sh`: the plain SIGKILL case alone with the script's release order as committed.

## WP-22 close-out (2026-11-03): the re-run on `6b258408`

One throwaway Hetzner Cloud cpx42 in fsn1 (Ubuntu 24.04, runsc `precopy-20261102.1`, image
`ghcr.io/de0ch/jarvis3-sandbox:2eb5bf396b5f`; musl release builds of `6b258408`), every migration `LOOPBACK=1`,
deleted after the run. `close-6b258408-logs/`: the queue's log and each step's console.

- `rmount-m2/`: `rig-mount.sh` (items 1, 5).
- `kill-k5/`: `rig-kill.sh` (item 4), `uniques.json` from the corrected `rig-uniques.py`.
- `rig-M{1,2}-{t,s}-{write,create,mkdir,rename}/`: `RIG_FI=1 LOOPBACK=1 rig-batch.sh M 2` (item 3's resend rule), each
  with `uniques.json` (`rig-uniques.py supervisor.log ROUTE=calls.json`: item 4 across a migration).
- `rmig-e6/`, `rmig-m6/` (empty cache), `rmig-f4/` (no file report: `rigreport.py` timed out), `rmig-f5/` (`FILL=1`):
  `rig-migcc.sh` (item 3 with Claude Code mid-turn).
- `rsess-s3/` (item 2), `rsess-s4/` (`STRACE=1`, item 9: `inventory-mount.json`), `rtomb-t5/`, `rtomb-t6/`.
- `rlat-l6/`: `rig-latency.sh` (items 6, 10).

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

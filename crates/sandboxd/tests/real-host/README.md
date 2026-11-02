# sandboxd on real runsc hosts (WP-19 review, 2026-11-02)

Checks that need the real runtime (DE0CH/gvisor `precopy-20261102.1`) rather than soak.py's stand-ins, run on two
throwaway Hetzner ccx13 hosts (2 dedicated vCPU, 8 GiB, Ubuntu 24.04): `a` set up with `fabric/host/setup.sh hub`,
`b` with `setup.sh burst` plus a hand-written host.json (placeholder ranges: hosts 10.99.0.0/24 on wg0, a = .1,
b = .10, sandboxes 10.98.0.0/16; the hosts reach each other over a private network, 10.77.0.0/24, b's WireGuard
endpoint being a's private address). The sandbox image is hand-made (`setup-host.sh`): python:3.12-slim's rootfs with
`crates/supervisor/spikes/fuse-gate/sandbox/mapcount.py` as the first process, which writes a counter into each of 16
pages of a scratch file (`/tmp/mc/counter`) through a `MAP_SHARED` mapping and checks every iteration that the mapping
holds what it last wrote. Every call goes through sandboxd's API with hermit's client certificate, as hermit's
migrate task makes them (spec, receive, send, `receive?wait=`).

| Script (runs on a) | What |
|---|---|
| `setup-host.sh a\|b` | the host setup above |
| `mig.sh NAME N [FILL_MIB]` | one live migration a -> b; FILL_MIB of random data in the scratch first; one JSON line with sandboxd's send statistics and the number of mapping faults the process saw on the target |
| `kill-sentry.sh NAME N` | SIGKILL of the sandbox's sentry (the `runsc-sandbox` process), then how long until `GET /v1/sandboxes` lists `alive:false` |
| `oom.sh NAME N` | a process inside a 512 MiB sandbox allocates past the limit (the host memcg's OOM kill) |

## Results (`results/`)

**The final scratch delta (GATE.md "WP-57", the short read).** With the quick check on the final delta (a control
build of this tree without `--checksum`): 3 of 3 migrations lost writes, every one of the 16 pages read back an older
counter on the target (e.g. 4292 where 4459 was written), `finalFiles` 0 (`mapcount-quick.jsonl`). With
`--checksum` (aviary eedfbda): 0 faults in 11 of 11, `finalFiles` 1 (the counter file, 65536 bytes)
(`mapcount-checksum.jsonl` and the first run). Stopped time (`stoppedMs`: from the checkpoint's return to SBXDONE)
with a near-empty scratch: 129-139 ms checksummed against 133-147 ms quick.

**Added stopped time on a full scratch** (`scratch-4800mib.jsonl`): 4800 MiB of random data in the 5 GiB scratch,
alternating builds on the same hosts: `--checksum` 15453 and 15785 ms stopped, the quick check 137 and 129 ms (and
lost the counter's writes, 16 faults each). With 2000 MiB: 5909 ms. So the checksum adds about 3.2 ms per MiB of
scratch in use on these hosts (rsync 3.2.7 with xxh128 on both ends: the time is reading every file on both hosts;
b had about 1 GiB of page cache, so its side read from disk). The live rounds took 43-46 s for the 4.8 GiB.

**The source while its send runs** (`source-alive-during-send.txt`, a 2000 MiB scratch, `GET /v1/sandboxes` on a
every 0.5 s): `running true` until the checkpoint returned, then `running false` for the 6.8 s of the final delta and
hand-over, then absent.

**hermit 13.2 item 6 on a real runsc.** A SIGKILL of the sentry empties the sandbox's cgroup at once (gofer and
systrap stubs exit with it; `runsc state` says `stopped`); `alive:false` 7.6-8.1 s later in 3 of 3 (the 10 s sampler)
(`kill-sentry.txt`). A memcg OOM inside the sandbox's limit kills a systrap stub (the kernel's `gvisor_sentry` task
holding the application's memory), not the sentry: the allocating process exits 137, the sandbox and its first
process keep running, and the cgroup stays `populated 1` with the sentry in it, so `alive:true` is right
(`memcg-oom.txt`).

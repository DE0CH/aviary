#!/usr/bin/env python3
"""Supervisor spec 22 item 4: `unique` never repeats on the connection across supervisor restarts and a migration.
Reads the spike's ops.tsv (t0_us, op, ino, path, detail, errno, dur_us, unique; one line per answered request, a
journal replay logs the replayed request again) and checks:
  - every unique names one request: all lines with that unique have the same op, node, path and detail (a replay
    after a restart re-executes the very same request; anything else is a repeat);
  - an INTERRUPT carries its target's unique + 1 (gVisor numbers requests in steps of 2, as Linux, and an interrupt
    reuses its target's id with the low bit set), so INTERRUPT lines are checked for that and left out of the rest;
  - across each boundary (a time in us: a supervisor start or the migration), every request first seen after it has
    a larger unique than every request first seen before it.
usage: uniques.py OPS_TSV [BOUNDARY_US ...]   exit 1 on a repeat or a boundary violation"""
import json, sys

ops, bounds = sys.argv[1], [int(b) for b in sys.argv[2:]]
first = {}   # unique -> (t0, request)
lines = 0
repeats = []
replays = 0
interrupts = []
for l in open(ops):
    f = l.rstrip("\n").split("\t")
    if len(f) < 8 or not f[7].isdigit():
        continue
    lines += 1
    t0, u = int(f[0]), int(f[7])
    req = (f[1], f[2], f[3], f[4])
    if f[1] == "INTERRUPT":
        tgt = int(f[4].split()[-1]) if f[4].split()[-1:] and f[4].split()[-1].isdigit() else -1
        interrupts.append({"unique": u, "target": tgt, "target_plus_1": u == tgt + 1})
        continue
    if u in first:
        if first[u][1] == req:
            replays += 1
        else:
            repeats.append({"unique": u, "first": first[u][1], "again": req})
        first[u] = (min(first[u][0], t0), first[u][1])
    else:
        first[u] = (t0, req)
per = []
ok = not repeats and all(i["target_plus_1"] for i in interrupts)
for b in bounds:
    before = [u for u, (t, _) in first.items() if t < b]
    after = [u for u, (t, _) in first.items() if t >= b]
    mb, ma = (max(before) if before else None), (min(after) if after else None)
    good = mb is None or ma is None or ma > mb
    ok = ok and good
    per.append({"boundary_us": b, "requests_before": len(before), "max_unique_before": mb,
                "requests_after": len(after), "min_unique_after": ma, "increasing": good})
print(json.dumps({"lines": lines, "uniques": len(first), "replayed_lines": replays, "repeats": repeats[:20],
                  "repeat_count": len(repeats), "interrupts": interrupts, "boundaries": per, "ok": ok}))
sys.exit(0 if ok else 1)

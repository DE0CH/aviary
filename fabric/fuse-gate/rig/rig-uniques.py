#!/usr/bin/env python3
"""Supervisor 22 item 4: a FUSE `unique` never repeats, across supervisor restarts and migrations, and no `opId`
names two different operations.
usage: rig-uniques.py SUPERVISOR_LOG [ROUTE=CALLS_JSON ...]
SUPERVISOR_LOG is the keeper's and supervisor's console of a fault-injection build (which logs every request it
reads as "fuse request"). Per connection (`boot`) the requests' uniques never repeat; a supervisor instance (from
one "fuse connection" line to the next) only reads uniques above every unique an earlier instance read; a
"journal replay" names a unique an earlier instance read and never reads again. CALLS_JSON files are fuse-rig's
/calls/ROUTE answers: every opId's calls name the same route and the same operation (path, offset, names), so a
retry or a replay is the same call and no unique was reused for another one.
Prints a JSON summary; exits 1 on any violation."""
import json, sys

def lines(path):
    for l in open(path, errors="replace"):
        l = l.strip()
        if l.startswith("{"):
            try:
                yield json.loads(l)
            except ValueError:
                pass

bad = []
instances = []  # per "fuse connection": {"boot", "read": [uniques], "replayed": [uniques]}
for e in lines(sys.argv[1]):
    m = e.get("msg")
    if m == "fuse connection":
        instances.append({"boot": e.get("boot"), "read": [], "replayed": []})
    elif instances and m == "fuse request":
        instances[-1]["read"].append(e["unique"])
    elif instances and m == "journal replay":
        instances[-1]["replayed"].append(e["unique"])

by_boot = {}
for i, inst in enumerate(instances):
    seen = by_boot.setdefault(inst["boot"], {"read": set(), "max": 0})
    dup = [u for u in inst["read"] if u in seen["read"]]
    if len(set(inst["read"])) != len(inst["read"]):
        dup += [u for u in set(inst["read"]) if inst["read"].count(u) > 1]
    if dup:
        bad.append({"instance": i, "repeated": sorted(set(dup))[:20]})
    low = [u for u in inst["read"] if u <= seen["max"]]
    if low:
        bad.append({"instance": i, "read_at_or_below_an_earlier_instance": sorted(low)[:20], "earlier_max": seen["max"]})
    unknown = [u for u in inst["replayed"] if u not in seen["read"]]
    if unknown:
        bad.append({"instance": i, "replayed_a_unique_never_read": unknown})
    seen["read"].update(inst["read"])
    if inst["read"]:
        seen["max"] = max(seen["max"], max(inst["read"]))

ops = {}
for arg in sys.argv[2:]:
    route, path = arg.split("=", 1)
    for c in json.load(open(path)):
        q = dict(c.get("query") or {})
        b = dict(c.get("body") or {}) if isinstance(c.get("body"), dict) else {}
        op = q.get("opId") or b.get("opId")
        if not op:
            continue
        for d in (q, b):
            for k in ("opId", "mtimeNs", "turnId"):
                d.pop(k, None)
        key = json.dumps({"route": route, "query": q, "body": b, "bytes": c.get("bytes")}, sort_keys=True)
        ops.setdefault(op, set()).add(key)
reused = {op: sorted(k) for op, k in ops.items() if len(k) > 1}
if reused:
    bad.append({"opids_naming_two_operations": dict(list(reused.items())[:10])})

summary = {
    "instances": len(instances),
    "boots": len(by_boot),
    "requests_read": sum(len(i["read"]) for i in instances),
    "replays": sum(len(i["replayed"]) for i in instances),
    "opids": len(ops),
    "violations": bad,
}
print(json.dumps(summary))
sys.exit(1 if bad else 0)

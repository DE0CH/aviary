#!/usr/bin/env python3
"""After a session: every file under the mount compared with the fake magpie's bytes, JSONL validity of transcripts,
and per-opcode counts with error answers. usage: report.py MOUNT MAGPIE_D OPS_TSV"""
import collections, hashlib, json, os, sys

mount, store, ops = sys.argv[1:4]
files = []
for root, dirs, names in os.walk(mount):
    for n in names:
        p = os.path.join(root, n)
        rel = os.path.relpath(p, mount)
        a = open(p, "rb").read()
        sp = os.path.join(store, rel)
        b = open(sp, "rb").read() if os.path.exists(sp) else None
        e = {"path": rel, "size": len(a), "equal_to_magpie": a == b, "sha256": hashlib.sha256(a).hexdigest()[:16]}
        if rel.endswith(".jsonl"):
            bad = 0
            lines = a.split(b"\n")
            for l in lines:
                if l.strip():
                    try:
                        json.loads(l)
                    except Exception:
                        bad += 1
            e["lines"] = sum(1 for l in lines if l.strip())
            e["bad_lines"] = bad
            e["ends_with_newline"] = a.endswith(b"\n")
        files.append(e)
cnt = collections.Counter()
errs = collections.Counter()
for l in open(ops):
    f = l.rstrip("\n").split("\t")
    if len(f) < 7:
        continue
    cnt[f[1]] += 1
    if f[5] != "0":
        errs[(f[1], f[5])] += 1
print(json.dumps({"files": files, "opcodes": dict(cnt), "errors": {f"{k[0]} errno {k[1]}": v for k, v in errs.items()}}, indent=1))

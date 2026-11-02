#!/usr/bin/env python3
"""After a session on the real build (root in the sandbox): every file under ~/.claude compared with fake magpie's
copy (magpie scope) or found in the supervisor's local scope directory, JSONL validity of transcripts, and the
magpie calls on each transcript that are not plain appends (a write below the end, a truncate). usage: rigreport.py"""
import hashlib, json, os, urllib.parse, urllib.request

BASE = "http://127.0.0.1:7300"
MOUNT, LOCAL = "/home/sandbox/.claude", "/var/lib/supervisor/local"

def get(path):
    try:
        with urllib.request.urlopen(BASE + path, timeout=30) as r:
            return r.read()
    except urllib.error.HTTPError:
        return None

files = []
for root, dirs, names in os.walk(MOUNT):
    for n in names:
        p = os.path.join(root, n)
        rel = os.path.relpath(p, MOUNT)
        if os.path.islink(p):
            files.append({"path": rel, "symlink": os.readlink(p)}); continue
        a = open(p, "rb").read()
        m = get("/data?path=" + urllib.parse.quote(rel))
        e = {"path": rel, "size": len(a), "sha256": hashlib.sha256(a).hexdigest()[:16]}
        if m is not None:
            e["scope"], e["equal_to_magpie"] = "magpie", a == m
        else:
            lp = os.path.join(LOCAL, rel)
            e["scope"] = "local" if os.path.exists(lp) else "neither"
            if e["scope"] == "local":
                e["equal_to_local"] = a == open(lp, "rb").read()
        if rel.endswith(".jsonl"):
            lines = [l for l in a.split(b"\n") if l.strip()]
            bad = 0
            for l in lines:
                try:
                    json.loads(l)
                except Exception:
                    bad += 1
            e.update(lines=len(lines), bad_lines=bad, ends_with_newline=a.endswith(b"\n"))
        files.append(e)
calls = {r: json.loads(get("/calls/" + r) or b"[]") for r in ("cc_write", "cc_truncate", "cc_setattr", "cc_rename", "cc_unlink")}
rewrites = []
sizes = {}
for c in calls["cc_write"]:
    q = c.get("query") or {}
    path, off = q.get("path"), q.get("offset")
    if path and path.endswith(".jsonl") and off is not None:
        if path in sizes and int(off) < sizes[path]:
            rewrites.append({"path": path, "offset": off, "bytes": c.get("bytes"), "size_before": sizes[path]})
        sizes[path] = max(sizes.get(path, 0), int(off) + (c.get("bytes") or 0))
print(json.dumps({"files": files, "calls": {k: len(v) for k, v in calls.items()},
                  "transcript_rewrites": rewrites,
                  "truncates": [c.get("body") or c.get("query") for c in calls["cc_truncate"]],
                  "setattrs": [c.get("body") or c.get("query") for c in calls["cc_setattr"]],
                  "renames": [c.get("body") for c in calls["cc_rename"]]}, indent=1))

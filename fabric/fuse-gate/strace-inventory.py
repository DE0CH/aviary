#!/usr/bin/env python3
"""Gate item 9: every syscall that names a path under the mount (/home/sandbox/.claude), from the sentry's --strace
log, grouped by syscall, answer and path class, split by program. usage: [MOUNT=DIR] strace-inventory.py BOOT_LOG > inventory.json"""

import collections, json, os, re, sys
MOUNT = os.environ.get("MOUNT", "/home/sandbox/.claude")  # the local reference run: MOUNT=/tmp/lh/.claude
line_re = re.compile(r"\] \[ *(\d+): *(\d+)\] (.+?) X (\w+)\((.*)\) = (.*?) \((?:[\d.]+[µnm]?s)\)\s*$")
path_re = re.compile(re.escape(MOUNT) + r"(?=[/\s,\"\)\]]|$)(?:/[^\s,\"\)\]]*)?")

def klass(p):
    rel = p[len(MOUNT):].lstrip("/")
    if not rel:
        return "<root>"
    parts = rel.split("/")
    top = parts[0]
    if top != "projects" or len(parts) < 3:
        return top + ("/" if len(parts) > 1 else "")
    name = parts[-1]
    if len(parts) == 3:
        if name.endswith(".jsonl"):
            return "projects/<main transcript>"
        if ".jsonl.compact.tmp." in name:
            return "projects/<compact tmp>"
        if ".jsonl.superseded-" in name:
            return "projects/<superseded>"
        return "projects/<sid dir or other>"
    if parts[3] == "subagents":
        return "projects/<sid>/subagents/" + ("*.jsonl" if name.endswith(".jsonl") else "*.meta.json" if name.endswith(".meta.json") else "*")
    return "projects/<sid>/" + parts[3] + "/"

def answer(r):
    m = re.match(r"-1 errno=(\d+) \((.*)\)", r)
    return f"errno {m.group(1)} ({m.group(2)})" if m else "ok"

tgid_comm = {}
counts = collections.Counter()
examples = {}
for raw in open(sys.argv[1], errors="replace"):
    m = line_re.search(raw)
    if not m:
        continue
    tgid, tid, comm, sc, args, res = m.groups()
    if tgid == tid:
        tgid_comm[tgid] = comm
    paths = path_re.findall(args)
    if not paths:
        continue
    prog = tgid_comm.get(tgid, comm)
    if prog.startswith("fuse-gate"):
        if sc != "mount":
            continue
        prog = "supervisor"
    elif prog not in ("claude", "python3", "sh", "bash", "ls", "find", "tail", "cat", "od", "stat", "truncate", "sed"):
        prog = "claude" if comm.startswith(("Bun", "claude", "HTTP", "File")) else prog
    for k in sorted({klass(p) for p in paths}):
        key = (prog, sc, answer(res), k)
        counts[key] += 1
        examples.setdefault(key, raw.split("] ", 2)[-1][:300].strip())
rows = [{"program": p, "syscall": s, "answer": a, "path": k, "count": c, "example": examples[(p, s, a, k)]}
        for (p, s, a, k), c in sorted(counts.items())]
flag = [r for r in rows if any(e in r["answer"] for e in ("errno 38", "errno 95", "errno 1 ", "errno 18", "errno 22", "errno 5 "))]
print(json.dumps({"rows": rows, "flagged": flag,
                  "syscalls": sorted({r["syscall"] for r in rows if r["program"] == "claude"})}, indent=1, ensure_ascii=False))

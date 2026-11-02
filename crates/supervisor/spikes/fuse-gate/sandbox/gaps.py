#!/usr/bin/env python3
"""Gate item 10: token gaps and tool latency from the mock model's chunk log and the driver's stdout log.
  token latency  = arrival of a chunk's token on Claude Code's stdout - the mock's send time
  token gap      = time between consecutive arrivals within one response (the mock sends every DELAY ms)
  tool latency   = mock's tool_use response done -> the next request (tool run + transcript writes)
usage: gaps.py CHUNKS_LOG STDOUT_JSONL"""
import json, re, sys

sent, tool_done, recv = {}, {}, {}
ts = [json.loads(l)["t"] for l in open(sys.argv[2])]
lo, hi = min(ts) - 2_000_000, max(ts)
for l in open(sys.argv[1]):
    p = l.split()
    t = int(p[0])
    if not lo <= t <= hi:
        continue
    m = re.search(r"req=(\d+)", l)
    if not m:
        continue
    r = int(m.group(1))
    if "chunk=" in l:
        sent[re.search(r"token=(\S+)", l).group(1)] = t
    elif "done stop=tool_use" in l:
        tool_done[r] = t
    elif " recv " in l:
        recv[r] = t
arr = {}
for l in open(sys.argv[2]):
    o = json.loads(l)
    for tok in re.findall(r"tk\d+x\d+", o["line"]):
        if tok in sent and tok not in arr and '"text_delta"' in o["line"]:
            arr[tok] = o["t"]
lat = sorted((arr[k] - sent[k]) / 1000 for k in arr)
gaps = []
by_req = {}
for k in arr:
    r, i = map(int, re.match(r"tk(\d+)x(\d+)", k).groups())
    by_req.setdefault(r, []).append((i, arr[k]))
for r, v in by_req.items():
    v.sort()
    gaps += [(b[1] - a[1]) / 1000 for a, b in zip(v, v[1:])]
gaps.sort()
tools = sorted((recv[r + 1] - t) / 1000 for r, t in tool_done.items() if r + 1 in recv)
q = lambda a, f: round(a[min(len(a) - 1, int(f * len(a)))], 1) if a else None
print(json.dumps({"tokens": len(lat), "missing": len(sent) - len(arr),
                  "token_latency_ms": {"p50": q(lat, .5), "p99": q(lat, .99), "max": q(lat, 1)},
                  "token_gap_ms": {"p50": q(gaps, .5), "p99": q(gaps, .99), "max": q(gaps, 1)},
                  "tool_roundtrip_ms": {"n": len(tools), "p50": q(tools, .5), "p99": q(tools, .99), "max": q(tools, 1)}}))

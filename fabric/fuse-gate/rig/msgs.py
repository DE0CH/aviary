#!/usr/bin/env python3
"""Item 10 on the real build: Claude Code runs under the supervisor's argv (6.1: no --include-partial-messages), so
its stdout carries whole messages, not token deltas. Per mock response with text chunks: message latency = the
arrival on Claude Code's stdout (the cc-tap log) of the assistant message holding the response's tokens - the mock's
send time of its last chunk; spread = the mock's first-to-last chunk time (the pacing, 39 x 20 ms). Tool round trip
= the mock's tool_use response done -> the next request (the tool run plus its transcript writes), as gaps.py.
usage: msgs.py CHUNKS_LOG TAP_JSONL [SINCE_US UNTIL_US]"""
import json, re, sys

since = int(sys.argv[3]) if len(sys.argv) > 3 else 0
until = int(sys.argv[4]) if len(sys.argv) > 4 else 1 << 62
first, last, tool_done, recv = {}, {}, {}, {}
for l in open(sys.argv[1]):
    p = l.split()
    t = int(p[0])
    if not since <= t <= until:
        continue
    m = re.search(r"req=(\d+)", l)
    if not m:
        continue
    r = int(m.group(1))
    if "chunk=" in l:
        first.setdefault(r, t)
        last[r] = t
    elif "done stop=tool_use" in l:
        tool_done[r] = t
    elif " recv " in l:
        recv[r] = t
arr = {}
for l in open(sys.argv[2]):
    o = json.loads(l)
    if not since <= o["t"] <= until + 60_000_000 or '"type":"assistant"' not in o["line"]:
        continue
    for r in {int(x) for x in re.findall(r"tk(\d+)x\d+", o["line"])}:
        arr.setdefault(r, o["t"])
lat = sorted((arr[r] - last[r]) / 1000 for r in arr if r in last)
spread = sorted((last[r] - first[r]) / 1000 for r in arr if r in last)
tools = sorted((recv[r + 1] - t) / 1000 for r, t in tool_done.items() if r + 1 in recv)
q = lambda a, f: round(a[min(len(a) - 1, int(f * len(a)))], 1) if a else None
print(json.dumps({"messages": len(lat), "missing": len([r for r in last if r not in arr]),
                  "message_latency_ms": {"p50": q(lat, .5), "p99": q(lat, .99), "max": q(lat, 1)},
                  "chunk_spread_ms": {"p50": q(spread, .5), "max": q(spread, 1)},
                  "tool_roundtrip_ms": {"n": len(tools), "p50": q(tools, .5), "p99": q(tools, .99), "max": q(tools, 1)}}))

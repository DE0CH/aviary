#!/usr/bin/env python3
"""Write latency/throughput of appends, as Claude Code does them (O_APPEND, one write per line), and a cold read.
usage: bench.py DIR N SIZE [read FILE]"""
import json, os, sys, time

d, n, size = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
os.makedirs(d, exist_ok=True)
p = os.path.join(d, f"bench-{n}x{size}.jsonl")
buf = (b"x" * (size - 1)) + b"\n"
fd = os.open(p, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
lat = []
t0 = time.perf_counter()
for _ in range(n):
    t = time.perf_counter()
    os.write(fd, buf)
    lat.append((time.perf_counter() - t) * 1000)
total = time.perf_counter() - t0
os.close(fd)
lat.sort()
q = lambda f: round(lat[min(len(lat) - 1, int(f * len(lat)))], 3)
out = {"file": p, "n": n, "size": size, "total_s": round(total, 3), "MBps": round(n * size / total / 1e6, 2),
       "p50_ms": q(0.5), "p90_ms": q(0.9), "p99_ms": q(0.99), "max_ms": round(lat[-1], 3)}
if len(sys.argv) > 5 and sys.argv[4] == "read":
    t = time.perf_counter()
    nb = 0
    with open(sys.argv[5], "rb") as f:
        while True:
            b = f.read(1 << 20)
            if not b:
                break
            nb += len(b)
    out["read"] = {"file": sys.argv[5], "bytes": nb, "s": round(time.perf_counter() - t, 3)}
print(json.dumps(out))

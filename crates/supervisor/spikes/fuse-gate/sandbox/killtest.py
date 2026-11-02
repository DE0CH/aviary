#!/usr/bin/env python3
"""Gate item 4, the caller's side: N appends of SIZE bytes (each a distinct fill byte) or N reads of a file, each
timed; then the file is checked: every chunk present exactly once, in order. usage:
  killtest.py write PATH N SIZE      killtest.py read PATH SIZE (reads PATH in SIZE chunks, checks against a pattern)"""
import json, os, sys, time

mode, path = sys.argv[1], sys.argv[2]
if mode == "write":
    n, size = int(sys.argv[3]), int(sys.argv[4])
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    lat = []
    for i in range(n):
        b = bytes([65 + i]) * (size - 1) + b"\n"
        t = time.perf_counter()
        w = os.write(fd, b)
        lat.append({"i": i, "returned": w, "ms": round((time.perf_counter() - t) * 1000, 1)})
    os.close(fd)
    data = open(path, "rb").read()
    exp = b"".join(bytes([65 + i]) * (size - 1) + b"\n" for i in range(n))
    print(json.dumps({"writes": lat, "file_size": len(data), "expected_size": len(exp), "exact": data == exp}))
else:
    size = int(sys.argv[3])
    lat = []
    fd = os.open(path, os.O_RDONLY)
    off = 0
    ok = True
    i = 0
    while True:
        t = time.perf_counter()
        b = os.pread(fd, size, off)
        ms = round((time.perf_counter() - t) * 1000, 1)
        if not b:
            break
        exp = bytes([97 + (i % 26)]) * len(b)
        ok = ok and b == exp
        lat.append({"i": i, "got": len(b), "ms": ms})
        off += len(b)
        i += 1
    print(json.dumps({"reads": lat, "bytes": off, "exact": ok}))

#!/usr/bin/env python3
"""Writes a counter through a MAP_SHARED mapping of a scratch file and checks, every iteration, that the mapping
still holds what was last written (WP-57: a migration that copies the scratch file from an older state shows here).

Usage: mapcount.py FILE OUT   (runs until killed; OUT gets one JSON line per second and one per fault)
The counter is written into the first 8 bytes of each of the file's 16 pages, as the supervisor's journal writes
its slots' `len` fields through its mapping (supervisor 9.9).
"""
import json
import mmap
import os
import struct
import sys
import time

path, out = sys.argv[1], sys.argv[2]
PG, PAGES = 4096, 16
fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
os.ftruncate(fd, PG * PAGES)
m = mmap.mmap(fd, PG * PAGES, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE)


def log(rec):
    with open(out, "a") as f:
        f.write(json.dumps(rec) + "\n")


i, faults, last = 0, 0, 0.0
log({"start": time.time(), "pid": os.getpid()})
while True:
    for p in range(PAGES):
        got = struct.unpack_from("<Q", m, p * PG)[0]
        if got != i:
            faults += 1
            log({"fault": "mapping", "page": p, "want": i, "got": got, "t": time.time()})
        struct.pack_into("<Q", m, p * PG, i + 1)
    i += 1
    time.sleep(0.001)
    now = time.time()
    if now - last >= 1:
        last = now
        log({"alive": now, "i": i, "faults": faults})

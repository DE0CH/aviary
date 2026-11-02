#!/usr/bin/env python3
"""Checks, across a live migration, that memory the program zeroed or freed reads as zero (WP-57).

Usage: stale.py OUT [MIB]   (runs until killed; OUT gets one JSON line per second and one per fault)
       stale.py --dirty MIB    (rewrites the same MIB MiB of its memory in a loop: keeps pre-copy from converging, so the
                                last live round is large and the time between it and the stop is long)

Every iteration:
  fresh:  mmap MIB MiB of new anonymous memory (in one mapping, then up to 256 mappings each of 2 MiB, 64 KiB and 4 KiB),
          check each is all zero (the kernel's promise for a new mapping), fill it with a non-zero byte, munmap it
          (the sentry frees the pages);
  zeroed: a mapping kept for the whole run is checked against what was last written to it (all zero on odd
          iterations), then written again: a non-zero byte on even iterations, zeroes on odd ones;
  dontneed: a mapping kept for the whole run is filled, then madvise(MADV_DONTNEED) (it must read as zero
          afterwards), checked at the start of the next iteration.
A page found with bytes it should not have is a "fault" line: the kind, the iteration, the number of 4 KiB pages
that are wrong and the first wrong byte. A migration that keeps pages a program zeroed or freed shows here.
"""
import json
import mmap
import os
import sys
import time

if sys.argv[1] == "--dirty":
    size = int(sys.argv[2]) << 20
    buf = bytearray(size)
    pats = (bytes([0x5a]) * size, bytes([0xa5]) * size)  # read only: every pass dirties exactly buf
    while True:
        for p in pats:
            buf[:] = p

out = sys.argv[1]
mib = int(sys.argv[2]) if len(sys.argv) > 2 else 64
n = mib << 20
quarter = n // 4
PG = 4096
zero_n = bytes(n)
zero_q = bytes(quarter)


def wrong_pages(buf, want_zero, byte):
    bad, first = 0, None
    for off in range(0, len(buf), PG):
        page = buf[off:off + PG]
        ok = page == zero_q[:PG] if want_zero else page.count(byte) == PG
        if not ok:
            bad += 1
            if first is None:
                first = next(b for b in page if b != (0 if want_zero else byte))
    return bad, first


def log(rec):
    with open(out, "a") as f:
        f.write(json.dumps(rec) + "\n")


zeroed = mmap.mmap(-1, quarter, flags=mmap.MAP_PRIVATE | mmap.MAP_ANONYMOUS)
dontneed = mmap.mmap(-1, quarter, flags=mmap.MAP_PRIVATE | mmap.MAP_ANONYMOUS)
zeroed_want = 0  # what zeroed holds: 0 = zeroes, else that byte
dontneed_armed = False
i = 0
faults = 0
last = 0.0
log({"start": time.time(), "pid": os.getpid(), "mib": mib})
while True:
    i += 1
    b = (i % 250) + 1
    # fresh: one mapping of MIB MiB, then up to 256 mappings each of 2 MiB, 64 KiB and 4 KiB
    for size in (n, 2 << 20, 64 << 10, PG):
        maps = []
        for _ in range(min(256, max(1, n // size))):
            m = mmap.mmap(-1, size, flags=mmap.MAP_PRIVATE | mmap.MAP_ANONYMOUS)
            cur = m[:]
            if cur != zero_n[:size]:
                bad, first = wrong_pages(cur, True, 0)
                faults += 1
                log({"fault": "fresh", "size": size, "i": i, "t": time.time(), "pages": bad, "first_byte": first})
            m.write(bytes([b]) * size)
            maps.append(m)
        for m in maps:
            m.close()
    # zeroed
    cur = zeroed[:]
    if (zeroed_want == 0 and cur != zero_q) or (zeroed_want != 0 and cur.count(zeroed_want) != quarter):
        bad, first = wrong_pages(cur, zeroed_want == 0, zeroed_want)
        faults += 1
        log({"fault": "zeroed", "i": i, "t": time.time(), "want": zeroed_want, "pages": bad, "first_byte": first})
    zeroed_want = 0 if i % 2 else b
    zeroed.seek(0)
    zeroed.write(bytes([zeroed_want]) * quarter)
    # dontneed
    cur = dontneed[:] if dontneed_armed else zero_q
    if cur != zero_q:
        bad, first = wrong_pages(cur, True, 0)
        again, _ = wrong_pages(dontneed[:], True, 0)
        faults += 1
        log({"fault": "dontneed", "i": i, "t": time.time(), "pages": bad, "first_byte": first, "pages_on_reread": again})
    dontneed.seek(0)
    dontneed.write(bytes([b]) * quarter)
    dontneed.madvise(mmap.MADV_DONTNEED)
    dontneed_armed = True
    now = time.time()
    if now - last >= 1:
        last = now
        log({"alive": now, "i": i, "faults": faults})

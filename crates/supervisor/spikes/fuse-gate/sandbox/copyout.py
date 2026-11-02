#!/usr/bin/env python3
"""Are pages the sentry writes into a process (a read(2)'s copy-out) carried across a pre-copy migration?
A writer process streams 4 KiB blocks (each starting with its sequence number) into a pipe; this process reads each
block straight into page (seq mod N) of a 32 MiB buffer with readv (only the sentry writes those pages), then notes
the block's seq in a list of its own (memory the application writes itself). Every 0.25 s it compares every page's
seq with the noted one and logs a JSON line; a page whose content is older than the note after a restore is a page
written by the sentry that the migration did not re-send. usage: copyout.py SECONDS LOG"""
import json, os, struct, sys, time

secs, logp = float(sys.argv[1]), sys.argv[2]
N, B = 8192, 4096
r, w = os.pipe()
if os.fork() == 0:
    os.close(r)
    seq = 0
    blk = bytearray(B)
    try:
        while True:
            struct.pack_into("<Q", blk, 0, seq)
            blk[8:16] = blk[0:8]
            os.write(w, blk)
            seq += 1
    except BrokenPipeError:
        os._exit(0)
os.close(w)
buf = bytearray(N * B)
mv = memoryview(buf)
noted = [-1] * N
log = open(logp, "w", buffering=1)
t0 = last = time.time()
seq = reads = 0
while time.time() - t0 < secs:
    for _ in range(2000):
        p = seq % N
        got = 0
        while got < B:
            got += os.readv(r, [mv[p * B + got:(p + 1) * B]])
        s = struct.unpack_from("<Q", buf, p * B)[0]
        noted[p] = s
        seq = s + 1
        reads += 1
    now = time.time()
    if now - last >= 0.25:
        bad = [p for p in range(N) if noted[p] >= 0 and struct.unpack_from("<Q", buf, p * B)[0] != noted[p]]
        log.write(json.dumps({"t": round(now, 3), "reads": reads, "seq": seq, "stale_pages": len(bad),
                              "examples": [(p, struct.unpack_from("<Q", buf, p * B)[0], noted[p]) for p in bad[:3]]}) + "\n")
        last = now
os.close(r)
os.wait()

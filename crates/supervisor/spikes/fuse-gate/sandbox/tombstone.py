#!/usr/bin/env python3
"""Claude Code 2.1.304's tombstone removal (performRemoveByUuid, read from the binary) replayed on a transcript:
open r+, read the last 64 KiB, ftruncate at the start of the line holding UUID, pwrite the rest of the tail back.
usage: tombstone.py TRANSCRIPT [UUID]   (default: the last assistant line's uuid)"""
import json, os, sys

p = sys.argv[1]
fd = os.open(p, os.O_RDWR)
size = os.fstat(fd).st_size
win = min(65536, size)
tail = os.pread(fd, win, size - win)
lines = tail.split(b"\n")
uuid = sys.argv[2] if len(sys.argv) > 2 else None
mode = "paged"
if uuid is None:
    # the last line with a uuid that starts inside the window (a message just streamed sits at the tail)
    for l in reversed(lines[1:]):
        try:
            m = json.loads(l)
        except Exception:
            continue
        if m.get("uuid"):
            uuid = m["uuid"]
            break
if uuid is None:
    for l in reversed(os.pread(fd, size, 0).split(b"\n")):
        try:
            m = json.loads(l)
        except Exception:
            continue
        if m.get("type") == "assistant":
            uuid = m["uuid"]
            break
key = b'"uuid":"%s"' % uuid.encode()
start = tail.find(key)
if start >= 0 and tail.rfind(b"\n", 0, start) >= 0:
    ls = tail.rfind(b"\n", 0, start) + 1
    le = tail.find(b"\n", start) + 1
    cut = size - win + ls
    rest = tail[le:]
    os.ftruncate(fd, cut)
    os.pwrite(fd, rest, cut)
    removed = le - ls
else:
    # not in the tail: the whole-file rewrite (files under 50 MiB)
    mode = "whole-file"
    data = os.pread(fd, size, 0)
    out = b"".join(l + b"\n" for l in data.split(b"\n") if l and key not in l)
    os.ftruncate(fd, 0)
    os.pwrite(fd, out, 0)
    removed = size - len(out)
os.close(fd)
print(json.dumps({"uuid": uuid, "mode": mode, "size_before": size, "removed_bytes": removed, "size_after": os.stat(p).st_size}))

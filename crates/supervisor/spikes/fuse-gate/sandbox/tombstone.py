#!/usr/bin/env python3
"""A REPLAY, not Claude Code: 2.1.304's tombstone removal (performRemoveByUuid, read from the binary) done by this
script on a transcript (Claude Code's own trigger was not reproduced, GATE.md item 2):
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
    # exactly the line whose own uuid is UUID goes; every other byte (empty lines, other lines naming the uuid as a
    # parent) stays as it was
    data = os.pread(fd, size, 0)
    def own(l):
        try:
            return json.loads(l).get("uuid") == uuid
        except Exception:
            return False
    parts = data.split(b"\n")
    out = b"\n".join(l for l in parts if not (key in l and own(l)))
    os.ftruncate(fd, 0)
    os.pwrite(fd, out, 0)
    removed = size - len(out)
os.close(fd)
print(json.dumps({"uuid": uuid, "mode": mode, "size_before": size, "removed_bytes": removed, "size_after": os.stat(p).st_size}))

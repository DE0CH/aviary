#!/usr/bin/env python3
"""Fills the content cache through the mount: COUNT files of 4 MiB under projects/fill/tool-results/ plus one main
transcript-sized file of MB megabytes (1 MiB lines). usage: fill.py COUNT MB"""
import os, sys
count, mb = int(sys.argv[1]), int(sys.argv[2])
d = "/home/sandbox/.claude/projects/fill/tool-results"
os.makedirs(d, exist_ok=True)
line = (b"y" * ((1 << 20) - 1)) + b"\n"
for i in range(count):
    with open(f"{d}/r{i}.txt", "wb") as f:
        for _ in range(4):
            f.write(line)
fd = os.open("/home/sandbox/.claude/projects/fill/big.jsonl", os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
for _ in range(mb):
    os.write(fd, line)
os.close(fd)
print("filled", count * 4 + mb, "MiB")

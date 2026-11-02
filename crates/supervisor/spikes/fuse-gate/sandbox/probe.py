#!/usr/bin/env python3
"""How long a stat of another directory and a read of another (cached) file on the mount wait right now."""
import json, os, time
t = time.time()
os.stat("/home/sandbox/.claude/projects")
a = time.time() - t
t = time.time()
open("/home/sandbox/.claude/keeper-probe.txt", "rb").read()
print(json.dumps({"start": round(time.time(), 3), "stat_s": round(a, 3), "read_s": round(time.time() - t, 3)}))

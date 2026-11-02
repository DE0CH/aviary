#!/usr/bin/env python3
"""The stdout tap of cc-tap/claude: copies stdin to stdout line by line, unbuffered, and appends each line with its
arrival time (us) to LOG as {"t", "line"}. usage: stamp.py LOG"""
import json, sys, time

out = sys.stdout.buffer
log = open(sys.argv[1], "a")
for line in sys.stdin.buffer:
    out.write(line)
    out.flush()
    log.write(json.dumps({"t": time.time_ns() // 1000, "line": line.decode(errors="replace").rstrip("\n")}) + "\n")
    log.flush()

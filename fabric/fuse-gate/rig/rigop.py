#!/usr/bin/env python3
"""One filesystem operation, its result as JSON on stdout (errno instead of an exception).
usage: rigop.py write PATH NBYTES | create PATH | mkdir PATH | rename SRC DST
write: one write(2) of NBYTES (a pattern, newline-terminated) with O_APPEND to an existing file; the result has the
count written and the bytes' sha256 (so the copy in magpie can be compared)."""
import hashlib, json, os, sys, time

def pattern(n):
    line = b"x" * 127 + b"\n"
    return (line * (n // len(line) + 1))[:n - 1] + b"\n"

op = sys.argv[1]
res = {"op": op, "args": sys.argv[2:], "start": time.time()}
try:
    if op == "write":
        data = pattern(int(sys.argv[3]))
        fd = os.open(sys.argv[2], os.O_WRONLY | os.O_APPEND)
        res["ret"] = os.write(fd, data)
        os.close(fd)
        res["sha256"] = hashlib.sha256(data).hexdigest()
        res["ok"] = res["ret"] == len(data)
    elif op == "create":
        fd = os.open(sys.argv[2], os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.close(fd)
        res["ok"] = True
    elif op == "mkdir":
        os.mkdir(sys.argv[2], 0o700)
        res["ok"] = True
    elif op == "rename":
        os.rename(sys.argv[2], sys.argv[3])
        res["ok"] = True
    else:
        raise SystemExit("unknown op " + op)
except OSError as e:
    res["ok"] = False
    res["errno"] = e.errno
    res["error"] = os.strerror(e.errno)
res["end"] = time.time()
print(json.dumps(res))

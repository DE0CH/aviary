#!/usr/bin/env python3
"""A 1 MiB append that a signal interrupts while the supervisor holds it: with SA_RESTART (restart=1) the sentry
restarts the write itself; without it the write fails EINTR and Python (PEP 475) calls it again. usage:
sigwrite.py PATH restart|eintr   (prints the result; the host side sends SIGUSR1 while the write is held)"""
import json, os, signal, sys, time

path, mode = sys.argv[1], sys.argv[2]
got = []
signal.signal(signal.SIGUSR1, lambda s, f: got.append(time.time()))
signal.siginterrupt(signal.SIGUSR1, mode != "restart")
open("/tmp/sigwrite.pid", "w").write(str(os.getpid()))
fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
t = time.time()
n = os.write(fd, b"S" * ((1 << 20) - 1) + b"\n")
os.close(fd)
print(json.dumps({"mode": mode, "returned": n, "s": round(time.time() - t, 3), "signals": len(got),
                  "size": os.stat(path).st_size}))

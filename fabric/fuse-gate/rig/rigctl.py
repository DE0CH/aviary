#!/usr/bin/env python3
"""fuse-rig's admin port from inside the sandbox, and small helpers.
usage: rigctl.py get PATH | post PATH | wait-held N SECS | wait-mount SECS | kill-supervisor | supervisor-pid | kill-cc"""
import json, os, sys, time, urllib.request

BASE = "http://127.0.0.1:7300"

def call(method, path):
    req = urllib.request.Request(BASE + path, method=method, data=b"" if method == "POST" else None)
    try:
        with urllib.request.urlopen(req, timeout=10) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()

def pids_of(*comms):
    out = []
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                if open(f"/proc/{p}/comm").read().strip() in comms:
                    out.append(int(p))
            except OSError:
                pass
    return out

def supervisor_pids():
    return pids_of("supervisor")

def cc_pids():
    return pids_of("claude", "claude.exe")

cmd = sys.argv[1]
if cmd in ("get", "post"):
    st, body = call(cmd.upper(), sys.argv[2])
    sys.stdout.write(body.decode(errors="replace") if body else "")
    sys.exit(0 if st < 400 else 1)
elif cmd == "wait-held":
    n, end = int(sys.argv[2]), time.time() + float(sys.argv[3])
    while time.time() < end:
        st, b = call("GET", "/held")
        if st == 200 and json.loads(b)["held"] >= n:
            print(b.decode()); sys.exit(0)
        time.sleep(0.05)
    print(call("GET", "/held")[1].decode()); sys.exit(1)
elif cmd == "wait-mount":
    end = time.time() + float(sys.argv[2])
    while time.time() < end:
        if any(" /home/sandbox/.claude fuse" in l for l in open("/proc/mounts")):
            try:
                os.listdir("/home/sandbox/.claude"); sys.exit(0)
            except OSError:
                pass
        time.sleep(0.05)
    sys.exit(1)
elif cmd == "kill-supervisor":
    for p in supervisor_pids():
        os.kill(p, 9)
    print(json.dumps(supervisor_pids()))
elif cmd == "kill-cc":
    # Claude Code killed (SIGKILL) while idle, as a crash; waits until it is gone
    killed = cc_pids()
    for p in killed:
        os.kill(p, 9)
    end = time.time() + 10
    while cc_pids() and time.time() < end:
        time.sleep(0.05)
    print(json.dumps({"killed": killed, "left": cc_pids()}))
elif cmd == "supervisor-pid":
    print(json.dumps(supervisor_pids()))
else:
    raise SystemExit("unknown command " + cmd)

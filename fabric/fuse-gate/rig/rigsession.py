#!/usr/bin/env python3
"""Drives Claude Code the way magpie does, through fuse-rig's admin port (run as root in a RIG=1 sandbox): one
start_turn command per prompt, then waits for the turn's end report (turn_state completed, failed or canceled).
The real supervisor launches Claude Code (supervisor 6.1-6.4) and reports. SCRIPT: {"env": {...} merged into the
descriptor's env first (a new etag: the supervisor ends an idle generation at the next start_turn, so the next
prompt starts a new process with --resume), "turns": [...], "resume": true to resume the conversation from the first
turn (the provider session id = the conversation id, as --session-id made it)}. Writes OUT/summary.json.
usage: rigsession.py SCRIPT.json OUT_DIR"""
import json, os, sys, time, urllib.request

BASE = "http://127.0.0.1:7300"

def call(method, path, body=None):
    data = json.dumps(body).encode() if body is not None else (b"" if method == "POST" else None)
    req = urllib.request.Request(BASE + path, method=method, data=data, headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        b = r.read()
        return json.loads(b) if b else None

def now_us():
    return time.time_ns() // 1000

script = json.load(open(sys.argv[1]))
out = sys.argv[2]
os.makedirs(out, exist_ok=True)
cid = open("/run/rig/cid").read().strip()
if script.get("env"):
    call("POST", "/env", script["env"])
resume = cid if script.get("resume") else None
summary = {"conversation": cid, "turns": []}
t0 = now_us()
for prompt in script["turns"]:
    ts = now_us()
    t = call("POST", "/turn", {"text": prompt, "resume": resume})
    tid, end, states = t["turnId"], None, []
    deadline = time.time() + script.get("turn_timeout_s", 900)
    while time.time() < deadline:
        states = [c["body"] for c in call("GET", "/calls/turn_state") if c["params"].get("turnId") == tid]
        end = next((s for s in states if s["state"] in ("completed", "failed", "canceled")), None)
        if end:
            break
        time.sleep(0.02)
    summary["turns"].append({"prompt": prompt[:200], "turnId": tid, "start_us": ts, "end_us": now_us(),
                             "state": end and end["state"], "error": end and end.get("error"),
                             "exit_code": end and end.get("exitCode"), "duration_ms": end and end.get("durationMs"),
                             "reports": [s["state"] for s in states]})
    if not end:
        break
    resume = cid
summary["total_ms"] = (now_us() - t0) // 1000
json.dump(summary, open(os.path.join(out, "summary.json"), "w"), indent=1)
print(json.dumps(summary))

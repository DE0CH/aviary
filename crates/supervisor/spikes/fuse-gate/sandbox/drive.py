#!/usr/bin/env python3
"""Drives Claude Code over stream-json, one prompt per turn; logs every stdout line with its arrival time (us).
usage: drive.py SCRIPT.json OUT_DIR   (SCRIPT: {"args": [...], "env": {...}, "turns": [...], "resume": sid|null})"""
import json, os, subprocess, sys, time

def now_us():
    return time.time_ns() // 1000

script = json.load(open(sys.argv[1]))
out = sys.argv[2]
os.makedirs(out, exist_ok=True)
env = dict(os.environ)
env.update({
    "HOME": "/home/sandbox",
    "ANTHROPIC_BASE_URL": "http://127.0.0.1:7200",
    "ANTHROPIC_API_KEY": "gate-dummy-key",
    "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
    "DISABLE_AUTOUPDATER": "1",
    "DISABLE_TELEMETRY": "1",
    "DISABLE_ERROR_REPORTING": "1",
})
env.update(script.get("env", {}))
args = ["claude", "-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose",
        "--include-partial-messages", "--dangerously-skip-permissions", "--model", "claude-sonnet-4-5"]
if script.get("resume"):
    args += ["--resume", script["resume"]]
args += script.get("args", [])
log = open(os.path.join(out, "stdout.jsonl"), "w")
err = open(os.path.join(out, "stderr.log"), "w")
t0 = now_us()
p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=err, env=env, cwd=script.get("cwd", "/home/sandbox"))
summary = {"args": args, "turns": [], "session_id": None}
for n_turn, prompt in enumerate(script["turns"]):
    ts = now_us()
    ctl = prompt.get("control") if isinstance(prompt, dict) else None
    if ctl is not None:
        # a control request (e.g. {"subtype": "set_cwd", "directory": ...}), answered by a control_response
        msg = {"type": "control_request", "request_id": f"gate-{n_turn}", "request": ctl}
        prompt = json.dumps(ctl)
    else:
        msg = {"type": "user", "message": {"role": "user", "content": prompt}}
    p.stdin.write((json.dumps(msg) + "\n").encode())
    p.stdin.flush()
    result = None
    while True:
        line = p.stdout.readline()
        if not line:
            break
        t = now_us()
        log.write(json.dumps({"t": t, "line": line.decode(errors="replace").rstrip("\n")}) + "\n")
        log.flush()
        try:
            m = json.loads(line)
        except Exception:
            continue
        if m.get("session_id"):
            summary["session_id"] = m["session_id"]
        if m.get("type") == "result" and ctl is None:
            result = m
            break
        if m.get("type") == "control_response" and ctl is not None:
            r = m.get("response", {})
            result = {"subtype": r.get("subtype", "success"), "is_error": r.get("subtype") == "error", "control": r}
            break
    summary["turns"].append({"prompt": prompt[:200], "start_us": ts, "end_us": now_us(),
                             "result": (result or {}).get("subtype"), "is_error": (result or {}).get("is_error"),
                             **({"control": result.get("control")} if result and "control" in result else {})})
    if result is None:
        break
p.stdin.close()
try:
    rc = p.wait(timeout=120)
except subprocess.TimeoutExpired:
    p.kill()
    rc = "killed"
for line in p.stdout:
    log.write(json.dumps({"t": now_us(), "line": line.decode(errors="replace").rstrip("\n")}) + "\n")
summary["exit"] = rc
summary["total_ms"] = (now_us() - t0) // 1000
json.dump(summary, open(os.path.join(out, "summary.json"), "w"), indent=1)
print(json.dumps(summary))

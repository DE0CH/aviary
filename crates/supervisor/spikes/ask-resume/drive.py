# Decision 17 (supervisor 7.6): does a resumed Claude Code take the answer to a question its predecessor asked?
# Phase 1 runs the supervisor's argv (AskUserQuestion allowed), lets the model ask, and ends the process the way END
# says: "stdin" (the supervisor's stop: close stdin, 5 s, SIGTERM, 3 s, SIGKILL), "term" (SIGTERM to the group,
# stdin open) or "kill" (SIGKILL). Phase 2 resumes the session and delivers the answer as MODE says: "control" (the
# control_response to the old request_id), "message" (the next user message, as text) or "tool_result" (a user
# message holding a tool_result block for the question's tool_use_id).
import glob, json, os, queue, signal, subprocess, sys, threading, time

CLAUDE, WORK, PORT, MODE, END = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4], sys.argv[5]
SID = "0190f6c2-0000-7000-8000-0000000000a7"
HOME = f"{WORK}/home"
os.makedirs(HOME, exist_ok=True)
os.makedirs(f"{WORK}/ws", exist_ok=True)
ENV = {"HOME": HOME, "PATH": "/usr/bin:/bin", "ANTHROPIC_BASE_URL": f"http://127.0.0.1:{PORT}",
       "ANTHROPIC_AUTH_TOKEN": "sandbox", "ANTHROPIC_MODEL": "claude-sonnet-4-5",
       "ANTHROPIC_SMALL_FAST_MODEL": "claude-haiku-4-5", "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC": "1",
       "DISABLE_AUTOUPDATER": "1", "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1",
       "CLAUDE_CODE_ATTACHMENTS_BY_PATH": "1"}
# the supervisor's argv (6.1) with AskUserQuestion allowed
BASE = [CLAUDE, "--output-format", "stream-json", "--verbose", "--input-format", "stream-json",
        "--permission-prompt-tool", "stdio", "--permission-mode", "bypassPermissions", "--setting-sources=",
        "--disallowedTools", "EnterPlanMode"]


def start(args, tag):
    p = subprocess.Popen(BASE + args, cwd=f"{WORK}/ws", env=ENV, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                         stderr=open(f"{WORK}/{tag}.stderr", "wb"), start_new_session=True)
    q = queue.Queue()
    log = open(f"{WORK}/{tag}.stdout.ndjson", "w")

    def rd():
        for line in p.stdout:
            log.write(line.decode())
            log.flush()
            try:
                q.put(json.loads(line))
            except ValueError:
                pass
        q.put(None)
    threading.Thread(target=rd, daemon=True).start()
    return p, q


def send(p, obj):
    p.stdin.write((json.dumps(obj) + "\n").encode())
    p.stdin.flush()


def until(q, pred, t):
    end = time.time() + t
    while time.time() < end:
        try:
            m = q.get(timeout=max(0.05, end - time.time()))
        except queue.Empty:
            break
        if m is None:
            return None
        if pred(m):
            return m
    return None


def init(p, q):
    send(p, {"type": "control_request", "request_id": "init-1", "request": {"subtype": "initialize"}})
    until(q, lambda m: m.get("type") == "control_response", 30)


def stop(p, how):
    if how == "kill":
        os.killpg(p.pid, signal.SIGKILL)
        p.wait()
        return "SIGKILL"
    if how == "stdin":
        p.stdin.close()
        try:
            p.wait(5)
            return "exited on stdin close"
        except subprocess.TimeoutExpired:
            pass
    os.killpg(p.pid, signal.SIGTERM)
    try:
        p.wait(3)
        return "exited on SIGTERM"
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        p.wait()
        return "SIGKILL after SIGTERM"


def requests():
    return len(glob.glob(f"{WORK}/req-*.json"))


def lines():
    path = glob.glob(f"{HOME}/.claude/projects/*/{SID}.jsonl")[0]
    out = []
    for raw in open(path):
        if not raw.strip():
            continue
        j = json.loads(raw)
        if j.get("type") not in ("user", "assistant"):
            continue
        c = (j.get("message") or {}).get("content")
        if isinstance(c, list):
            c = [(b.get("type"), str(b.get("text") or b.get("content") or b.get("name") or "")[:100]) for b in c]
        out.append((j["type"], (j.get("message") or {}).get("model"), c))
    return out


# phase 1: the question
p, q = start(["--session-id", SID], "p1")
init(p, q)
send(p, {"type": "user", "message": {"role": "user", "content": "ask me a colour"}, "parent_tool_use_id": None})
ask = until(q, lambda m: m.get("type") == "control_request" and m["request"].get("subtype") == "can_use_tool", 60)
assert ask, "no can_use_tool"
r = ask["request"]
print("phase 1: can_use_tool", json.dumps({k: r.get(k) for k in ("tool_name", "tool_use_id", "requires_user_interaction")}))
time.sleep(1)
how = stop(p, END)
time.sleep(1)
print(f"phase 1: ended by {END}: {how}, exit {p.returncode}; model requests so far: {requests()}")
print("phase 1: transcript:", lines())

# phase 2: resume and answer
p, q = start(["--resume", SID], f"p2-{MODE}")
init(p, q)
upd = dict(r["input"])
upd["answers"] = {"Which colour?": "Blue"}
if MODE == "control":
    send(p, {"type": "control_response", "response": {"subtype": "success", "request_id": ask["request_id"],
             "response": {"behavior": "allow", "updatedInput": upd, "toolUseID": r["tool_use_id"]}}})
    m = until(q, lambda m: m.get("type") in ("assistant", "user", "result", "control_request"), 15)
    print("phase 2 control: output within 15 s of the late answer:", None if m is None else m.get("type"))
elif MODE == "message":
    send(p, {"type": "user", "parent_tool_use_id": None, "message": {"role": "user",
             "content": 'Your questions have been answered: "Which colour?"="Blue". You can now continue with these answers in mind.'}})
elif MODE == "tool_result":
    send(p, {"type": "user", "parent_tool_use_id": None, "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": r["tool_use_id"],
         "content": 'Your questions have been answered: "Which colour?"="Blue".'}]}})
res = until(q, lambda m: m.get("type") == "result", 60)
print(f"phase 2 {MODE}: result", None if res is None else json.dumps({k: res.get(k) for k in ("subtype", "is_error")}),
      f"model requests: {requests()}")
stop(p, "stdin")
print(f"phase 2 {MODE}: transcript:", lines())
last = sorted(glob.glob(f"{WORK}/req-*.json"), key=lambda f: int(f.rsplit("-", 1)[1].split(".")[0]))[-1]
msgs = json.load(open(last))["messages"]


def shown(m):
    # the environment's own system reminders (paths, tool lists) are left out
    c = m["content"]
    if isinstance(c, list):
        c = [{k: v for k, v in b.items() if k != "cache_control"} for b in c
             if not (b.get("type") == "text" and b.get("text", "").startswith("<system-reminder>"))]
    return {"role": m["role"], "content": c}


print(f"phase 2 {MODE}: the last model request's last two messages:", json.dumps([shown(m) for m in msgs[-2:]])[:1200])

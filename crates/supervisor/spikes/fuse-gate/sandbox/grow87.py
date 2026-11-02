#!/usr/bin/env python3
"""Grows a real Claude Code transcript to MB MiB with valid, chained lines: pairs of an assistant tool_use and its
user tool_result whose `toolUseResult` carries 1 MiB of output (stored in the transcript, not sent to the model: the
shape of today's large transcripts, whose bulk is tool output and images), then one closing assistant text line.
The template fields come from the transcript's own last user and assistant lines. usage: grow87.py TRANSCRIPT MB"""
import json, os, sys, uuid

p, mb = sys.argv[1], int(sys.argv[2])
lines = [json.loads(l) for l in open(p, "rb").read().split(b"\n") if l.strip()]
chain = [m for m in lines if m.get("type") in ("user", "assistant") and m.get("uuid")]
leaf = chain[-1]["uuid"]
ua = next(m for m in reversed(chain) if m["type"] == "user")
aa = next(m for m in reversed(chain) if m["type"] == "assistant")
blob = ("0123456789abcdef" * 64 + "\n") * 1023
fd = os.open(p, os.O_WRONLY | os.O_APPEND)
n = 0
while os.fstat(fd).st_size < mb << 20:
    a_id, u_id, tid = str(uuid.uuid4()), str(uuid.uuid4()), f"toolu_grow{n:06d}"
    a = dict(aa, parentUuid=leaf, uuid=a_id, message=dict(aa["message"], id=f"msg_grow{n:06d}",
             content=[{"type": "tool_use", "id": tid, "name": "Bash", "input": {"command": "true", "description": "grow"}}],
             stop_reason="tool_use"))
    u = dict(ua, parentUuid=a_id, uuid=u_id, sourceToolAssistantUUID=a_id,
             message={"role": "user", "content": [{"type": "tool_result", "tool_use_id": tid, "content": "ok"}]},
             toolUseResult={"stdout": blob, "stderr": "", "interrupted": False, "isImage": False})
    for k in ("promptId",):
        u.pop(k, None)
    os.write(fd, (json.dumps(a, separators=(",", ":")) + "\n" + json.dumps(u, separators=(",", ":")) + "\n").encode())
    leaf, n = u_id, n + 1
last = dict(aa, parentUuid=leaf, uuid=str(uuid.uuid4()), message=dict(aa["message"], id="msg_grow_end",
            content=[{"type": "text", "text": "grown"}], stop_reason="end_turn"))
os.write(fd, (json.dumps(last, separators=(",", ":")) + "\n").encode())
print(json.dumps({"pairs": n, "size": os.fstat(fd).st_size}))
os.close(fd)

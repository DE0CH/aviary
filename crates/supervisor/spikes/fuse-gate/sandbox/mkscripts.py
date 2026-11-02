#!/usr/bin/env python3
"""Writes the scripted sessions the gate drives (into DIR): session-a.json (tools with file history, subagent, failure
injection incl. a model fallback, relocation onto an occupied destination, growth past 5 MiB, /compact with local GC),
session-b.json and session-b2.json (resumes of a after a torn tail), session-c.json (token-gap turns).
usage: mkscripts.py DIR"""
import json, os, sys

d = sys.argv[1]
def g(*lines):
    return "\n".join(lines)
tools = g(
    "Gate session, tools turn.",
    'GATE: tool Bash {"command":"mkdir -p /home/sandbox/work && echo hi > /home/sandbox/work/a.txt && ls -la ~/.claude","description":"setup"}',
    'GATE: tool Write {"file_path":"/home/sandbox/work/b.txt","content":"line1\\nline2\\n"}',
    'GATE: tool Read {"file_path":"/home/sandbox/work/b.txt"}',
    'GATE: tool Edit {"file_path":"/home/sandbox/work/b.txt","old_string":"line2","new_string":"line two"}',
    'GATE: tool ToolSearch {"query":"select:TodoWrite,Glob,Grep","max_results":5}',
    'GATE: tool TodoWrite {"todos":[{"content":"gate the mount","status":"in_progress","activeForm":"Gating the mount"}]}',
    'GATE: tool Glob {"pattern":"*.txt","path":"/home/sandbox/work"}',
    'GATE: tool Grep {"pattern":"line","path":"/home/sandbox/work","output_mode":"content"}',
    'GATE: tool Agent {"description":"gate subagent","prompt":"Subagent task.\\nGATE: tool Bash {\\"command\\":\\"echo from-subagent\\",\\"description\\":\\"sub\\"}\\nGATE: text 3 0","subagent_type":"general-purpose","run_in_background":false}',
    "GATE: text 20 0")
grow = g("Grow the transcript past 5 MiB.",
         *[f'GATE: tool Write {{"file_path":"/home/sandbox/work/big{i}.txt","content":"@@BIG:1024"}}' for i in range(7)],
         "GATE: text 5 0")
SID = "11111111-2222-4333-8444-555555555555"
# file history: in -p mode Claude Code 2.1.304 snapshots files before edits only with
# CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING (the Agent SDK sets it when asked); supervisor spec 6.2 does not set it
a = {"args": ["--session-id", SID, "--fallback-model", "claude-haiku-4-5"],
     "env": {"CLAUDE_CODE_TRANSCRIPT_LOCAL_GC": "1", "CLAUDE_STREAM_IDLE_TIMEOUT_MS": "3000",
             "CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING": "true"},
     "turns": [tools, g("Retry turn: the first attempt fails mid-stream.", "GATE: failonce t2", "GATE: text 8 0"),
               g("Reset stream: the orphaned message is tombstoned.", "GATE: trunconce t3 reset", "GATE: text 6 0"),
               g("Garbage in the stream.", "GATE: trunconce t4 garbage", "GATE: text 6 0"),
               g("Refusal: the refused message is retracted.", "GATE: trunconce t6 refusal", "GATE: text 6 0"),
               g("Model fallback: a thinking-only response fails mid-stream.", "GATE: thinkfail t7 api_error",
                 "GATE: text 4 0"),
               # relocation: the session moves to /home/sandbox/work; session.sh plants an occupant at the destination
               {"control": {"subtype": "set_cwd", "path": "/home/sandbox/work", "trust_accepted": True,
                            "trusted_directory": "/home/sandbox/work"}},
               grow, "/compact", g("After compaction.", "GATE: text 5 0")]}
b = {"env": {"CLAUDE_CODE_TRANSCRIPT_LOCAL_GC": "1"}, "resume": SID, "cwd": "/home/sandbox/work",
     "turns": [g("Resumed after a torn tail.", 'GATE: tool Bash {"command":"tail -c 300 ~/.claude/projects/-home-sandbox-work/*.jsonl | od -c | tail -5","description":"tail"}', "GATE: text 5 0")]}
b2 = dict(b, turns=[g("Resumed after a line torn mid-way.", "GATE: text 5 0")])
c = {"turns": [g(f"Token gap turn {i}.", 'GATE: tool Read {"file_path":"/home/sandbox/work/b.txt"}',
                 'GATE: tool Glob {"pattern":"*.txt","path":"/home/sandbox/work"}', "GATE: text 40 20") for i in range(5)]}
for n, s in (("session-a", a), ("session-b", b), ("session-b2", b2), ("session-c", c)):
    json.dump(s, open(os.path.join(d, n + ".json"), "w"), indent=1)

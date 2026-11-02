#!/usr/bin/env python3
"""The gate's scripted sessions (crates/supervisor/spikes/fuse-gate/sandbox/mkscripts.py) for rigsession.py, under
the supervisor's own argv, environment and managed files (supervisor 6.1-6.4): cwd /workspace, no --fallback-model,
no relocation (no set_cwd control request in 6.1-7), the variables the gate passed on the command line set in the
descriptor's env instead (SandboxSelf.env, which the supervisor applies unchanged). Writes into DIR:
a.json (tools, subagent, failure injection, growth past 5 MiB, /compact with CLAUDE_CODE_TRANSCRIPT_LOCAL_GC=1),
b.json and b2.json (resumes after a torn tail; a new process, since b's env changes the etag), c.json (item 10's
turns: Read, Glob, 40 chunks 20 ms apart, five turns), catalog.json (the tombstone trigger that got furthest in
the gate: two refusals with the refusal fallback armed from the model catalog), r0.json/r.json (item 6: a seed turn,
then a resume). usage: mksessions.py DIR"""
import json, os, sys

d = sys.argv[1]
def g(*lines):
    return "\n".join(lines)
W = "/workspace/work"
tools = g(
    "Gate session, tools turn.",
    f'GATE: tool Bash {{"command":"mkdir -p {W} && echo hi > {W}/a.txt && ls -la ~/.claude","description":"setup"}}',
    f'GATE: tool Write {{"file_path":"{W}/b.txt","content":"line1\\nline2\\n"}}',
    f'GATE: tool Read {{"file_path":"{W}/b.txt"}}',
    f'GATE: tool Edit {{"file_path":"{W}/b.txt","old_string":"line2","new_string":"line two"}}',
    'GATE: tool ToolSearch {"query":"select:TodoWrite,Glob,Grep","max_results":5}',
    'GATE: tool TodoWrite {"todos":[{"content":"gate the mount","status":"in_progress","activeForm":"Gating the mount"}]}',
    f'GATE: tool Glob {{"pattern":"*.txt","path":"{W}"}}',
    f'GATE: tool Grep {{"pattern":"line","path":"{W}","output_mode":"content"}}',
    'GATE: tool Agent {"description":"gate subagent","prompt":"Subagent task.\\nGATE: tool Bash {\\"command\\":\\"echo from-subagent\\",\\"description\\":\\"sub\\"}\\nGATE: text 3 0","subagent_type":"general-purpose","run_in_background":false}',
    'GATE: tool mcp__tools__echo {"text":"through the gateway"}',
    "GATE: text 20 0")
grow = g("Grow the transcript past 5 MiB.",
         *[f'GATE: tool Write {{"file_path":"{W}/big{i}.txt","content":"@@BIG:1024"}}' for i in range(7)],
         "GATE: text 5 0")
a = {"env": {"CLAUDE_CODE_TRANSCRIPT_LOCAL_GC": "1", "CLAUDE_STREAM_IDLE_TIMEOUT_MS": "3000"},
     "turns": [tools, g("Retry turn: the first attempt fails mid-stream.", "GATE: failonce t2", "GATE: text 8 0"),
               g("Reset stream: the orphaned message is tombstoned.", "GATE: trunconce t3 reset", "GATE: text 6 0"),
               g("Garbage in the stream.", "GATE: trunconce t4 garbage", "GATE: text 6 0"),
               g("Refusal: the refused message is retracted.", "GATE: trunconce t6 refusal", "GATE: text 6 0"),
               g("A thinking-only response fails mid-stream.", "GATE: thinkfail t7 api_error", "GATE: text 4 0"),
               grow, "/compact", g("After compaction.", "GATE: text 5 0")]}
b = {"env": {"RIG_RESTART": "b"}, "resume": True,
     "turns": [g("Resumed after a torn tail.", 'GATE: tool Bash {"command":"tail -c 300 ~/.claude/projects/-workspace/*.jsonl | od -c | tail -5","description":"tail"}', "GATE: text 5 0")]}
b2 = {"env": {"RIG_RESTART": "b2"}, "resume": True, "turns": [g("Resumed after a line torn mid-way.", "GATE: text 5 0")]}
c = {"resume": True,
     "turns": [g(f"Token gap turn {i}.", f'GATE: tool Read {{"file_path":"{W}/b.txt"}}',
                 f'GATE: tool Glob {{"pattern":"*.txt","path":"{W}"}}', "GATE: text 40 20") for i in range(5)]}
catalog = {"env": {"CLAUDE_CODE_REFUSAL_FALLBACK_CATCH_ALL": "1",
                   "CLAUDE_CODE_MODEL_CAPABILITIES": "claude-sonnet-4-5=refusal_fallback",
                   "ANTHROPIC_DEFAULT_OPUS_MODEL": "claude-opus-4-8"},
           "turns": ["First turn.\nGATE: text 3 0",
                     g("Refused twice.", "GATE: trunconce r1 refusal", "GATE: trunconce r2 refusal", "GATE: text 4 0"),
                     "After.\nGATE: text 2 0"]}
r0 = {"turns": [g("Seed.", 'GATE: tool Bash {"command":"echo seed","description":"seed"}', "GATE: text 3 0")]}
r = {"resume": True, "turns": [g("Resumed.", "GATE: text 3 0")]}
for n, s in (("a", a), ("b", b), ("b2", b2), ("c", c), ("catalog", catalog), ("r0", r0), ("r", r)):
    json.dump(s, open(os.path.join(d, n + ".json"), "w"), indent=1)

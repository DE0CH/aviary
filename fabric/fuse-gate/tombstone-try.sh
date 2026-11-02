#!/usr/bin/env bash
# Gate item 2, tombstone: tries to make Claude Code 2.1.304 tombstone a message it already wrote, with a fallback model
# configured (abandonAttempt fires on a model fallback, a refusal fallback or a streaming fallback, read from the
# binary). CASE thinkfail: a thinking-only response, then an api_error mid-stream (-> model fallback, "chain_advance");
# CASE refusal: a refusal with stop_details category cyber, and its continuation refused too (-> refusal fallback).
# Records the transcript's lines, the mock's log and every FUSE op on the transcript (SETATTR size = a truncate,
# a WRITE below the end = a rewrite). Usage: tombstone-try.sh NAME thinkfail|refusal
set -euo pipefail
cd "$(dirname "$0")"
name=$1; case_=$2; out=/root/out/tomb-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; ./gate.sh bundle "$name"; ./gate.sh run "$name"; sleep 1
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
case $case_ in
thinkfail) t2='Thinking then a server error.\nGATE: thinkfail tf1 api_error\nGATE: text 4 0'
           args='["--fallback-model", "claude-haiku-4-5"]' ;;
refusal)   t2='Refused twice.\nGATE: trunconce r1 refusalcat\nGATE: trunconce r2 refusalcat\nGATE: text 4 0'
           args='["--fallback-model", "claude-haiku-4-5", "--settings", "{\"switchModelsOnFlag\": true}"]' ;;
*) echo "unknown case $case_" >&2; exit 2 ;;
esac
x sh -c "cat > /tmp/t.json" <<J
{"args": $args, "env": {}, "turns": ["First turn.\nGATE: text 3 0", "$t2", "After.\nGATE: text 2 0"]}
J
x python3 /media/drive.py /tmp/t.json /tmp/out-t > "$out/summary.json"
# shellcheck disable=SC2016  # expanded by the sandbox's shell
x sh -c 'for f in ~/.claude/projects/*/*.jsonl; do python3 -c "
import json,sys
for l in open(sys.argv[1]):
    m=json.loads(l); c=m.get(\"message\",{}).get(\"content\")
    print(m.get(\"type\"), m.get(\"uuid\",\"\")[:8], (json.dumps(c)[:100] if c else m.get(\"subtype\",\"\")))
" $f; done' > "$out/transcript-lines.txt"
r cat /srv/model/chunks.log > "$out/model.log"
r sh -c "grep -P '\tprojects/[^/]*/[^/]*\.jsonl\t' /srv/supervisor/ops.tsv | grep -v -P '\t(LOOKUP|GETATTR)\t'" > "$out/transcript-ops.tsv"
x sh -c 'cat /tmp/out-t/stdout.jsonl' > "$out/stdout.jsonl"
r python3 /media/report.py /home/sandbox/.claude /srv/magpie/d /srv/supervisor/ops.tsv > "$out/report.json"
echo "truncates of the transcript: $(grep -c -P '\tSETATTR\t.*size=' "$out/transcript-ops.tsv" || true)"
grep -P '\tSETATTR\t|\tREAD\t' "$out/transcript-ops.tsv" | cut -f2,5 | tail -n 3
grep -v '^attachment\|^atis\|^api-req\|^last-prompt\|^queue' "$out/transcript-lines.txt"
./gate.sh rm "$name"

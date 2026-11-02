#!/usr/bin/env bash
# Supervisor 22 item 2, the tombstone, on the real build (GATE.md open item 3): the gate's furthest trigger
# (catalog.json: two refusals with the refusal fallback armed from the model catalog and the catch-all route on, set
# through the descriptor's env) under the supervisor's argv. A tombstone shows as a magpie write below a transcript's
# end or a truncate of it (rigreport.py transcript_rewrites, truncates). Results: /root/out/rtomb-NAME/.
# Usage: rig-tomb.sh NAME [SESSION]   (SESSION: a mksessions.py name, default catalog)
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
name=$1; sess=${2:-catalog}; out=/root/out/rtomb-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; rm -rf "/var/lib/gate/scratch/$name"; mkdir -p /var/log/gate; : > "/var/log/gate/$name.log"
RIG=1 RIG_CC_TAP=1 LAYOUT=${LAYOUT:-scratch} ./gate.sh bundle "$name"; ./gate.sh run "$name"
r() { ./gate.sh exec "$name" --root "$@"; }
r python3 /media/rig/rigctl.py wait-mount 60 || { echo "FAIL: no mount"; exit 1; }
r sh -c "mkdir -p /tmp/s && python3 /media/rig/mksessions.py /tmp/s"
r python3 /media/rig/rigsession.py "/tmp/s/$sess.json" /tmp/out-t > "$out/summary.json"
# shellcheck disable=SC2016  # expanded by the sandbox's shell
r sh -c 'for f in /home/sandbox/.claude/projects/*/*.jsonl; do python3 -c "
import json,sys
for l in open(sys.argv[1]):
    m=json.loads(l); c=m.get(\"message\",{}).get(\"content\")
    print(m.get(\"type\"), m.get(\"uuid\",\"\")[:8], (json.dumps(c)[:100] if c else m.get(\"subtype\",\"\")))
" $f; done' > "$out/transcript-lines.txt"
r python3 /media/rig/rigreport.py > "$out/report.json"
r cat /srv/model/chunks.log > "$out/model.log"
r cat /srv/log/supervisor.log > "$out/supervisor.log"
r cat /tmp/cc-tap/stdout.jsonl > "$out/cc-stdout.jsonl"
jq -c '{rewrites: .transcript_rewrites, truncates: .truncates}' "$out/report.json"
jq -c '[.turns[] | [.state, .error.code // null]]' "$out/summary.json"
grep -o 'model_refusal[a-z_]*\|retracted_message_uuids' "$out/cc-stdout.jsonl" | sort | uniq -c
./gate.sh rm "$name"

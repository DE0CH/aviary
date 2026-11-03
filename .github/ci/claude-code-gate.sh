#!/usr/bin/env bash
# claude-code-gate (supervisor 22, magpie 20; nightly.yml): one side of the gate. Runs the harness's `cc`, `e21` and
# `e22` scenarios with the sandbox image <image> and writes <out>/gate.json:
#   {"claudeCode": "<version>", "unclassified": [top-level names], "unanswered": [opcodes],
#    "counts": <the cc scenario's cc-counts: magpie's events by kind and the rows by kind>}
# from the sandbox Pods' supervisor logs (`unclassified path`, `unanswered opcode`) and the driver's output.
#   claude-code-gate.sh <sha12> <sandbox image> <out dir>
# The harness is left up for the artifacts step; run.sh down ends it. GATE_HARNESS_UP=1: it is already up with <image>.
set -euo pipefail
s=${1:?sha12}
image=${2:?sandbox image}
out=$(mkdir -p "${3:?out dir}" && cd "$3" && pwd)
version=$(docker run --rm --entrypoint /opt/claude-code/bin/claude "$image" --version | awk '{print $1}')
echo "claude-code-gate: Claude Code $version in $image"
if [ "${GATE_HARNESS_UP:-}" != 1 ]; then
  timeout 1500 testing/e2e/run.sh up "pigeon:$s" "magpie:$s" "hermit:$s" "aviary-testing:$s" "$image"
fi
set +e
timeout 2500 testing/e2e/run.sh test cc e21 e22 > "$out/driver.log" 2>&1
code=$?
set -e
grep -v '^in \|^out ' "$out/driver.log" | tail -40
# every sandbox Pod's log, right after the scenarios (before an idle release removes them)
: > "$out/sandboxes.log"
for p in $(kubectl -n aviary-e2e get pods -l app=sandbox -o name); do
  kubectl -n aviary-e2e logs "$p" --all-containers --tail=-1 >> "$out/sandboxes.log" 2>&1 || true
done
counts=$(sed -n 's/^cc-counts //p' "$out/driver.log" | tail -1)
[ -n "$counts" ] || counts=null
# the supervisor's JSON log lines (aviary-core log: `msg` and the event's fields at the top level)
field_of() { # field_of <msg> <field>: the distinct values, as a JSON array
  jq -R --arg m "$1" --arg f "$2" 'fromjson? | select(.msg == $m) | .[$f] // empty | tostring' "$out/sandboxes.log" | sort -u | jq -s .
}
jq -n --arg v "$version" --argjson counts "$counts" \
  --argjson unclassified "$(field_of 'unclassified path' path)" \
  --argjson unanswered "$(field_of 'unanswered opcode' opcode)" \
  '{claudeCode: $v, unclassified: $unclassified, unanswered: $unanswered, counts: $counts}' > "$out/gate.json"
cat "$out/gate.json"
[ "$code" = 0 ] || { echo "claude-code-gate: the scenarios failed with Claude Code $version" >&2; exit 1; }
[ "$counts" != null ] || { echo "claude-code-gate: no cc-counts in the driver's output" >&2; exit 1; }

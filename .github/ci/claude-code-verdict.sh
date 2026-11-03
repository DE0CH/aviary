#!/usr/bin/env bash
# claude-code-gate's verdict (supervisor 22): the newest Claude Code release against the pinned one. Fails on any
# `unclassified path`, any opcode the mount refused that the pinned release did not make it refuse, or any change in
# the events magpie derived or the rows of the `cc` conversation. Writes the comparison to the job summary.
#   claude-code-verdict.sh <pinned gate.json> <newest gate.json>
set -euo pipefail
p=${1:?pinned gate.json}
n=${2:?newest gate.json}
summary=${GITHUB_STEP_SUMMARY:-/dev/stdout}
{
  echo "### claude-code-gate: $(jq -r .claudeCode "$n") against the pinned $(jq -r .claudeCode "$p")"
  echo
  echo '```json'
  jq -n --slurpfile p "$p" --slurpfile n "$n" '{pinned: $p[0], newest: $n[0]}'
  echo '```'
} >> "$summary"
fail=0
if [ "$(jq '.unclassified | length' "$n")" != 0 ]; then
  echo "unclassified paths: $(jq -c .unclassified "$n")" >&2; fail=1
fi
new_ops=$(jq -c --slurpfile p "$p" '.unanswered - $p[0].unanswered' "$n")
if [ "$new_ops" != "[]" ]; then
  echo "opcodes the mount refused only for the newest release: $new_ops" >&2; fail=1
fi
if ! jq -e --slurpfile p "$p" '.counts == $p[0].counts' "$n" >/dev/null; then
  echo "event or row counts drifted: pinned $(jq -c .counts "$p"), newest $(jq -c .counts "$n")" >&2; fail=1
fi
if [ "$fail" = 0 ]; then echo "claude-code-gate: green, the pin may move to $(jq -r .claudeCode "$n")" | tee -a "$summary"; fi
exit "$fail"

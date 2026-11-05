#!/bin/bash
# Decision 17 (supervisor 7.6): whether a resumed Claude Code 2.1.304 takes the answer to a question asked before
# its process ended, and what ending the process does to an open question. No network: the model is model.py.
#   run.sh [CLAUDE]   (default: Claude Code 2.1.304 installed with npm into a temporary prefix)
# Output: the verdict lines of every combination; results.txt holds the run of 2026-11-05.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
claude=${1:-}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
if [ -z "$claude" ]; then
  npm install --silent --prefix "$tmp/cc" -g @anthropic-ai/claude-code@2.1.304 >/dev/null
  claude=$tmp/cc/bin/claude
fi
"$claude" --version
port=8117
for end in stdin term kill; do
  for mode in control message tool_result; do
    w=$tmp/$end-$mode
    mkdir -p "$w"
    python3 "$here/model.py" "$w" "$port" &
    model=$!
    sleep 0.5
    echo "== end of phase 1: $end; answer in phase 2: $mode"
    python3 "$here/drive.py" "$claude" "$w" "$port" "$mode" "$end" || echo "(failed)"
    kill "$model"
    wait "$model" 2>/dev/null || true
  done
done

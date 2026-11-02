#!/usr/bin/env bash
# The verdict of the frame check's self-test (.github/workflows/pigeon-app.yml): check-frames.sh was run with the seeded
# incompatible frame (test/incompatible-frame.ndjson) added to the corpus, and it must have rejected that frame and only
# that frame. Exit 1 alone is not enough: a crash of node, npm or check-frames.sh under set -e exits 1 too.
#
#   self-test-verdict.sh <check-frames exit status> <its output>
#
# Passes when the status is 1, every protocol line of validate-frames.mjs is `FAIL … 1 invalid`, and every failure line
# names the seeded frame (its file and case).
set -euo pipefail
status=$1
log=$2
seeded_file=incompatible-frame.ndjson
seeded_case='seeded incompatible frame'
fail() { echo "::error::the frame check's self-test: $1"; exit 1; }
[ "$status" = 1 ] || fail "check-frames.sh answered $status on a seeded incompatible frame (1 expected)"
protocols=$(grep -cE '^(FAIL|ok  ) protocol ' "$log" || true)
[ "$protocols" -gt 0 ] || fail "validate-frames.mjs printed no verdict (it did not run to the end)"
rejected=$(grep -cE '^FAIL protocol .*: [0-9]+ server frames, [0-9]+ client frames, 1 invalid$' "$log" || true)
[ "$rejected" = "$protocols" ] || fail "not every protocol build rejected exactly one frame: $(grep -E '^(FAIL|ok  ) protocol ' "$log" | tr '\n' ' ')"
failures=$(grep -E '^     \S+:[0-9]+ (in|out) ' "$log" || true)
[ -n "$failures" ] || fail "no failing frame was named"
others=$(grep -v -F "$seeded_file:" <<<"$failures" || true)
[ -z "$others" ] || fail "frames other than the seeded one were rejected: $others"
grep -q -F "($seeded_case" <<<"$failures" || fail "the rejected frame is not the seeded case"
echo "the frame check rejected the seeded frame ($seeded_file, '$seeded_case') and nothing else"

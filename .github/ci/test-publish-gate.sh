#!/usr/bin/env bash
# ci.yml's publish job runs for every main commit that passed the gates: a push to main, and a run started on main
# with workflow_dispatch (clients.yml's land job commits as github-actions[bot], whose push starts no workflow, then
# dispatches ci.yml on main; with publish on push only, a green bot head was never released, 2026-11-17). Never for
# a pull request or another branch.
#   bash .github/ci/test-publish-gate.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
cond=$(awk '/^  publish:/{p=1; next} p && /^  [a-z]/{exit} p && /^    if:/{sub(/^    if: */, ""); print; exit}' \
  "$here/../workflows/ci.yml")
[ -n "$cond" ] || { echo "FAIL: no if: on the publish job" >&2; exit 1; }
python3 -I - "$cond" <<'EOF'
import re, sys
cond = sys.argv[1]
def runs(event, ref):
    e = cond.replace("github.event_name", repr(event)).replace("github.ref", repr(ref))
    e = e.replace("&&", " and ").replace("||", " or ").replace("!=", "!= ").replace("'", '"')
    if re.search(r"[A-Za-z_]+\.[A-Za-z_]+|\$\{\{", e):
        sys.exit(f"FAIL: publish's if: uses something this test can't evaluate: {cond}")
    return eval(e, {"__builtins__": {}}, {})
want = {
    ("push", "refs/heads/main"): True,
    ("workflow_dispatch", "refs/heads/main"): True,
    ("push", "refs/heads/feature"): False,
    ("workflow_dispatch", "refs/heads/feature"): False,
    ("pull_request", "refs/pull/1/merge"): False,
    ("pull_request", "refs/heads/main"): False,
}
bad = [f"{ev} on {ref}: runs={runs(ev, ref)}, want {w}" for (ev, ref), w in want.items() if runs(ev, ref) != w]
if bad:
    sys.exit("FAIL: publish's if: " + cond + "\n  " + "\n  ".join(bad))
print("publish gate: ok")
EOF

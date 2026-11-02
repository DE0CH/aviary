#!/bin/bash
# gate item 7 (supervisor 22): with --setting-sources= a workspace settings hook and a workspace CLAUDE.md do not
# load; the managed CLAUDE.md and the managed PreToolUse SendUserFile hook do. Claude Code 2.1.304 with the
# supervisor's argv (6.1) and model environment (6.2), against a model stand-in (dump.py) that keeps each request
# body and answers the first turn with a SendUserFile tool_use, so the PreToolUse hooks run.
#
# Three runs:
#   test       the supervisor's managed settings (allowManagedHooksOnly, the managed hook) and --setting-sources=
#   control-a  no allowManagedHooksOnly, --setting-sources=project,user,local: the workspace CLAUDE.md and the
#              workspace hooks must load (the positive control: the check can see them)
#   control-b  no allowManagedHooksOnly, --setting-sources=: --setting-sources= alone keeps the workspace out
set -euo pipefail
cd "$(dirname "$0")"
docker build -q --network host -t wp23-gate7 . >/dev/null
one() {
  local mode=$1 sources=$2 only=$3
  rm -rf "out-$mode" && mkdir -p "out-$mode"
  docker run --rm --network none -e SOURCES="$sources" -e ONLY="$only" -v "$PWD:/gate:ro" -v "$PWD/out-$mode:/out" \
    wp23-gate7 bash -c '
set -e
mkdir -p /etc/claude-code /workspace/.claude
echo "MANAGED-MARKER-7f3: the managed instructions." > /etc/claude-code/CLAUDE.md
cat > /etc/claude-code/managed-settings.json <<J
{$([ "$ONLY" = 1 ] && echo "\"allowManagedHooksOnly\": true,") "cleanupPeriodDays": 36500,
 "hooks": {"PreToolUse": [{"matcher": "SendUserFile",
   "hooks": [{"type": "command", "command": "cat > /out/managed-hook-input.json", "timeout": 120}]}]}}
J
echo "WORKSPACE-MARKER-9c1: a model-written CLAUDE.md." > /workspace/CLAUDE.md
echo "a file to send" > /workspace/f.txt
cat > /workspace/.claude/settings.json <<J
{"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "touch /out/workspace-hook-ran"}]}],
           "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "touch /out/workspace-hook-ran"}]}],
           "PreToolUse": [{"matcher": "SendUserFile", "hooks": [{"type": "command", "command": "touch /out/workspace-hook-ran"}]}]}}
J
python3 /gate/dump.py & sleep 1
useradd -m -u 1000 -s /bin/bash sbx 2>/dev/null || true
chown -R 1000:1000 /workspace /out
cd /workspace
printf "%s\n" "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"send f.txt\"},\"parent_tool_use_id\":null}" | \
  timeout 90 setpriv --reuid 1000 --regid 1000 --clear-groups env -i HOME=/home/node PATH=/usr/bin:/bin \
  ANTHROPIC_BASE_URL=http://127.0.0.1:8099 ANTHROPIC_AUTH_TOKEN=sandbox ANTHROPIC_MODEL=claude-sonnet-4-5 \
  CLAUDE_CODE_ATTACHMENTS_BY_PATH=1 \
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1 \
  /opt/claude-code/bin/claude --output-format stream-json --verbose --input-format stream-json \
  --permission-prompt-tool stdio --permission-mode bypassPermissions "--setting-sources=$SOURCES" \
  --disallowedTools AskUserQuestion,EnterPlanMode,ExitPlanMode --session-id 0190f6c2-0000-7000-8000-000000000001 \
  > /out/stdout.ndjson 2>/out/stderr.log || true
'
  local o="out-$mode" m w t
  m=$(grep -l "MANAGED-MARKER-7f3" "$o"/req-*.json 2>/dev/null | wc -l || true)
  w=$(grep -l "WORKSPACE-MARKER-9c1" "$o"/req-*.json 2>/dev/null | wc -l || true)
  t=$(grep -l '"name":"SendUserFile"' "$o"/req-*.json 2>/dev/null | wc -l || true)
  echo "$mode (--setting-sources=$sources, allowManagedHooksOnly=$only): requests $(find "$o" -name "req-*.json" | wc -l);" \
    "managed CLAUDE.md in $m; workspace CLAUDE.md in $w; SendUserFile offered in $t;" \
    "workspace hook ran: $([ -e "$o/workspace-hook-ran" ] && echo yes || echo no);" \
    "managed SendUserFile hook ran: $([ -s "$o/managed-hook-input.json" ] && echo "yes ($(python3 -c 'import json,sys; d=json.load(open(sys.argv[1])); print(json.dumps({k: d.get(k) for k in ("hook_event_name","tool_name","tool_use_id","tool_input","cwd")}))' "$o/managed-hook-input.json"))" || echo no)"
}
one test "" 1
one control-a "project,user,local" 0
one control-b "" 0

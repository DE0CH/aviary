#!/bin/bash
# gate item 7 (supervisor 22): with --setting-sources= a workspace settings hook and a workspace CLAUDE.md do not
# load; the managed CLAUDE.md does. Claude Code 2.1.304 with the supervisor's argv (6.1) and model environment (6.2),
# against a recorder that keeps each request body.
set -euo pipefail
cd "$(dirname "$0")"
# SOURCES: the --setting-sources value (empty: the supervisor's; "project,user,local": the control, which must load both)
SOURCES=${SOURCES-}
docker build -q --network host -t wp23-gate7 . >/dev/null
rm -rf out && mkdir -p out
docker run --rm --network none -e SOURCES="$SOURCES" -v "$PWD:/gate:ro" -v "$PWD/out:/out" wp23-gate7 bash -c '
set -e
mkdir -p /etc/claude-code /workspace/.claude
echo "MANAGED-MARKER-7f3: the managed instructions." > /etc/claude-code/CLAUDE.md
cat > /etc/claude-code/managed-settings.json <<J
{"allowManagedHooksOnly": true, "cleanupPeriodDays": 36500}
J
echo "WORKSPACE-MARKER-9c1: a model-written CLAUDE.md." > /workspace/CLAUDE.md
cat > /workspace/.claude/settings.json <<J
{"hooks": {"SessionStart": [{"hooks": [{"type": "command", "command": "touch /out/workspace-hook-ran"}]}],
           "UserPromptSubmit": [{"hooks": [{"type": "command", "command": "touch /out/workspace-hook-ran"}]}]}}
J
python3 /gate/dump.py & sleep 1
useradd -m -u 1000 -s /bin/bash sbx 2>/dev/null || true
chown -R 1000:1000 /workspace /out
cd /workspace
printf "%s\n" "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"hello\"},\"parent_tool_use_id\":null}" | \
  timeout 60 setpriv --reuid 1000 --regid 1000 --clear-groups env -i HOME=/home/node PATH=/usr/bin:/bin \
  ANTHROPIC_BASE_URL=http://127.0.0.1:8099 ANTHROPIC_AUTH_TOKEN=sandbox ANTHROPIC_MODEL=claude-sonnet-4-5 \
  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1 DISABLE_AUTOUPDATER=1 DISABLE_TELEMETRY=1 DISABLE_ERROR_REPORTING=1 \
  /opt/claude-code/bin/claude --output-format stream-json --verbose --input-format stream-json \
  --permission-prompt-tool stdio --permission-mode bypassPermissions "--setting-sources=$SOURCES" \
  --disallowedTools AskUserQuestion,EnterPlanMode,ExitPlanMode --session-id 0190f6c2-0000-7000-8000-000000000001 \
  > /out/stdout.ndjson 2>/out/stderr.log || true
'
echo "requests: $(ls out/req-*.json 2>/dev/null | wc -l)"
m=$(grep -l "MANAGED-MARKER-7f3" out/req-*.json 2>/dev/null | wc -l || true)
w=$(grep -l "WORKSPACE-MARKER-9c1" out/req-*.json 2>/dev/null | wc -l || true)
echo "managed CLAUDE.md in requests: $m; workspace CLAUDE.md in requests: $w; workspace hook ran: $([ -e out/workspace-hook-ran ] && echo yes || echo no)"

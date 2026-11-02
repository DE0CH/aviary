#!/usr/bin/env bash
# Prepares a gate host's unpacked image for the Claude Code runs of the re-run (rig-session.sh, rig-latency.sh), the
# way supervisor 6.1 and 6.4 expect a sandbox image (WP-24's): Claude Code at /opt/claude-code/bin/claude (here a
# symlink to the image's own @anthropic-ai/claude-code 2.1.304 native binary) and the managed directory
# /etc/claude-code (a tmpfs in the bundle, which the supervisor writes). Also writes the stdout tap
# (/var/lib/gate/bin/rig/cc-tap/claude: RIG_CC_TAP=1 binds it over /opt/claude-code/bin), which stamps every line
# Claude Code writes to stdout with its arrival time for item 10. Run as root after host-setup.sh.
set -euo pipefail
root=$(readlink -f /var/lib/gate/image)/rootfs
exe=$(find "$root/usr" "$root/opt" -path '*/@anthropic-ai/claude-code/bin/claude.exe' -print -quit 2>/dev/null || true)
[ -n "$exe" ] || { echo "no claude.exe in the image" >&2; exit 1; }
ver=$(jq -r .version "$(dirname "$(dirname "$exe")")/package.json")
rel=/${exe#"$root"/}
mkdir -p "$root/opt/claude-code/bin" "$root/etc/claude-code"
ln -sfn "$rel" "$root/opt/claude-code/bin/claude"
mkdir -p /var/lib/gate/bin/rig/cc-tap
cat > /var/lib/gate/bin/rig/cc-tap/claude <<W
#!/bin/bash
# the stdout tap: Claude Code stays this process (exec); its stdout goes through stamp.py to the supervisor
mkdir -p /tmp/cc-tap
exec > >(exec python3 -u /media/rig/stamp.py /tmp/cc-tap/stdout.jsonl)
exec $rel "\$@"
W
chmod 755 /var/lib/gate/bin/rig/cc-tap/claude
echo "ok: /opt/claude-code/bin/claude -> $rel ($ver)"

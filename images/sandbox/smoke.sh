#!/usr/bin/env bash
# The sandbox image's smoke test (hermit spec 17, supervisor spec 22 "Image smoke", platform 6.1 `sandbox-smoke`),
# under runc on a Docker host. Two parts:
# 1. the image as built: entrypoint, users, folders, tools, Claude Code, the wrappers, sudo, apt as uid 1000 (needs
#    the internet: the Debian mirror), and the keeper started without a proxy credential;
# 2. the real thing: the image's keeper (PID 1) and supervisor against fuse-rig's world (fake hermit and fake
#    magpie behind the reference test gateway) and mock-model, all on the host's loopback (the container shares the
#    host's network, so the sandbox token's source address is 127.0.0.1). A magpie `start_turn` makes the supervisor
#    mount ~/.claude and launch Claude Code; the turn's model and MCP calls must reach mock-model and fake-tools
#    through the gateway's proxy as absolute-form `http://model/…` and `http://tools/mcp` requests (the container has
#    no address for either name, and only the gateway adds the verified `Aviary-Sandbox-Conversation` mock-model
#    files them under). Then Docker on demand through /run/supervisor/docker.sock, and a runtime stop.
# Usage: smoke.sh <image> <dir with the static mock-model and fuse-rig of the same commit>
# SMOKE_RUNTIME names a Docker runtime for part 2's sandbox container (ci.yml's `gvisor` job: runsc, the fork's
# release, so the supervisor's FUSE mount and Claude Code run in gVisor as on a host); SMOKE_PARTS=2 skips part 1.
# Uses 127.0.0.1 ports 7070 (the supervisor's), 7200 (mock-model) and 7300 (fuse-rig's admin port). The apt check's
# container uses Docker's default network unless SMOKE_INTERNET_NETWORK names another (`host` on an IPv6-only host).
# the checks' commands run in the container: their $ expand there, not here
# shellcheck disable=SC2016
set -uo pipefail
img=${1:?image}
bin=$(cd "${2:?dir with mock-model and fuse-rig}" && pwd)
claude_version=2.1.304
work=$(mktemp -d)
name=sandbox-smoke-$$
status=0
# shellcheck disable=SC2317 # run by the trap
cleanup() {
  docker rm -f -v "$name" "$name-nocred" >/dev/null 2>&1 || true
  jobs -p | xargs -r kill 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
ok() { echo "ok: $*"; }
fail() { echo "FAIL: $*"; status=1; }
check() { # check <what> <command…>: the command's success is the check
  local what=$1; shift
  if "$@"; then ok "$what"; else fail "$what"; fi
}
# a command in a throwaway container without the keeper: as root, or `-u 1000`, with no network unless asked; the
# sandbox's hostname and the /etc/hosts line the supervisor writes for it (DESIGN 12 D2), so sudo resolves it
sbx_host=(--hostname sandbox --add-host sandbox:127.0.1.1)
img_run() { docker run --rm --network none "${sbx_host[@]}" --entrypoint /bin/bash "$@"; }

if [ "${SMOKE_PARTS:-1 2}" != 2 ]; then
echo "== the image"
size=$(docker image inspect -f '{{.Size}}' "$img")
echo "size: $((size / 1024 / 1024)) MiB"
entry=$(docker image inspect -f '{{json .Config.Entrypoint}}' "$img")
cmd=$(docker image inspect -f '{{json .Config.Cmd}}' "$img")
user=$(docker image inspect -f '{{.Config.User}}' "$img")
check "the keeper is the entrypoint ($entry), no CMD ($cmd), run as root ($user)" \
  test "$entry|$cmd|${user:-root}" = '["/usr/local/bin/supervisor-keeper"]|null|root'
check "no PORT in the image's environment" \
  sh -c "! docker image inspect -f '{{json .Config.Env}}' '$img' | grep -q '\"PORT='"
speaks=$(docker image inspect -f '{{index .Config.Labels "dev.de0ch.aviary.speaks"}}' "$img")
label_sum=$(docker image inspect -f '{{index .Config.Labels "dev.de0ch.aviary.supervisor"}}' "$img")
image_sum=$(img_run "$img" -c 'sha256sum /usr/local/bin/supervisor | cut -d" " -f1')
check "label speaks $speaks names the three surfaces" \
  sh -c "echo '$speaks' | jq -e 'has(\"hermit-sandbox\") and has(\"magpie-sandbox\") and has(\"supervisor-control\")' >/dev/null"
check "label supervisor is the binary's sha256 ($image_sum)" test -n "$image_sum" -a "$label_sum" = "$image_sum"

check "no tini, no /opt/agent, no /run/sbx-docker" \
  img_run "$img" -c '! command -v tini && ! test -e /usr/bin/tini && ! test -e /opt/agent && ! test -e /run/sbx-docker'
check "supervisor, supervisor-keeper root 0755, sandbox-secret -> supervisor" img_run "$img" -c '
  for b in supervisor supervisor-keeper; do
    test "$(stat -c "%U %a" /usr/local/bin/$b)" = "root 755" || exit 1
  done
  test "$(readlink /usr/local/bin/sandbox-secret)" = supervisor'
check "supervisor --version" img_run "$img" -c '/usr/local/bin/supervisor --version | grep -q "^supervisor "'
# Claude Code: the path the supervisor executes resolves inside /opt/claude-code, and nothing on the way (every
# directory from / down, the link, the binary) nor anything under /opt/claude-code is uid 1000's or writable by it
check "/opt/claude-code/bin/claude resolves to a root-owned binary uid 1000 cannot change" img_run "$img" -c '
  set -e
  real=$(readlink -f /opt/claude-code/bin/claude)
  echo "  /opt/claude-code/bin/claude -> $real"
  case $real in /opt/claude-code/*) ;; *) exit 1 ;; esac
  test -x "$real"
  p=$real
  while [ "$p" != / ]; do
    test "$(stat -c %u "$p")" = 0
    if [ ! -L "$p" ]; then [ $(( 0$(stat -c %a "$p") & 022 )) = 0 ]; fi
    p=$(dirname "$p")
  done
  test -z "$(find /opt/claude-code \( ! -user root -o \( ! -type l -perm /022 \) \) -print -quit)"
  test "$(stat -c %u /opt/claude-code/bin/claude)" = 0'
check "Claude Code $claude_version runs as uid 1000" \
  img_run -u 1000 "$img" -c "/opt/claude-code/bin/claude --version | grep -qF '$claude_version'"
check "a login shell's claude is the same binary" \
  img_run -u 1000 "$img" -lc 'test "$(readlink -f "$(command -v claude)")" = "$(readlink -f /opt/claude-code/bin/claude)"'
check "uid 1000 is node, home /home/sandbox" \
  img_run "$img" -c 'test "$(getent passwd 1000 | cut -d: -f1,3,4,6)" = "node:1000:1000:/home/sandbox"'
check "/workspace, /work and an empty /home/sandbox/.claude are uid 1000's; /var/lib/supervisor root 0700" img_run "$img" -c '
  for d in /workspace /work /home/sandbox /home/sandbox/.claude; do test "$(stat -c %u "$d")" = 1000 || exit 1; done
  test -z "$(ls -A /home/sandbox/.claude)" && test "$(stat -c "%U %a" /var/lib/supervisor)" = "root 700"'
check "python3 is the venv's and imports pandas, weasyprint, pymupdf and reportlab (uid 1000)" \
  img_run -u 1000 "$img" -c 'test "$(command -v python3)" = /opt/py/bin/python3 && python3 -c "import pandas, weasyprint, pymupdf, reportlab, sbx_reportlab_cjk"'
check "the TrueType Noto Sans/Serif HK fonts" \
  img_run "$img" -c 'ls /usr/share/fonts/truetype/noto-hk/Noto{Sans,Serif}HK-{Regular,Bold}.ttf >/dev/null'
check "dockerd, containerd and buildx present" \
  img_run "$img" -c 'test -x /usr/bin/dockerd && test -x /usr/bin/containerd && /usr/bin/docker buildx version >/dev/null'
out=$(img_run -u 1000 "$img" -c 'docker ps' 2>&1)
code=$?
check "without the supervisor's socket \`docker ps\` says Docker is not available ($code: $out)" \
  test "$code" = 1 -a "$out" = "docker: Docker is not available in this sandbox"
check "apt-get and apt are the wrapper, also under sudo" img_run -u 1000 "$img" -c '
  test "$(command -v apt-get)" = /usr/local/bin/apt-get && test "$(command -v apt)" = /usr/local/bin/apt &&
  test "$(sudo -n sh -c "command -v apt-get")" = /usr/local/bin/apt-get'
check "sudo prints nothing on stderr (the hostname resolves)" img_run -u 1000 "$img" -c '
  err=$(sudo -n true 2>&1 >/dev/null) && test -z "$err" || { echo "  sudo said: $err"; exit 1; }'
check "sudo keeps HTTPS_PROXY" img_run -u 1000 -e HTTPS_PROXY=http://proxy.invalid:3128 "$img" \
  -c 'test "$(sudo -n printenv HTTPS_PROXY)" = http://proxy.invalid:3128'
check "the image's npm prefix and pip config hold with only PATH, LANG and HOME (supervisor 6.2)" img_run -u 1000 "$img" -c '
  test "$(env -i PATH="$PATH" HOME=/home/sandbox LANG=C.UTF-8 npm config get prefix)" = /home/sandbox/.npm-global &&
  env -i PATH="$PATH" HOME=/home/sandbox pip config list | grep -q "disable-pip-version-check"'
# the image's Env sets neither HOME nor NPM_CONFIG_PREFIX: root, as Docker starts it, has /root and node's prefix
check "the image's Env has no HOME and no NPM_CONFIG_PREFIX" \
  sh -c "! docker image inspect -f '{{json .Config.Env}}' '$img' | grep -qE '\"(HOME|NPM_CONFIG_PREFIX)='"
out=$(docker run --rm --network none --entrypoint npm "$img" config get prefix 2>&1)
check "root's npm (docker run --entrypoint npm, the image's environment) has prefix /usr/local ($out)" \
  test "$out" = /usr/local
homes=$(docker run --rm --network none --entrypoint /bin/sh -u 1000 "$img" -c 'echo "$HOME"')
homes="$homes $(docker run --rm --network none --entrypoint /bin/sh "$img" -c 'echo "$HOME"')"
check "uid 1000's HOME is /home/sandbox, root's /root, as Docker starts them ($homes)" \
  test "$homes" = "/home/sandbox /root"
# root's npm keeps node's prefix (/usr/local): `sudo npm install -g` puts nothing root-owned into uid 1000's prefix,
# and uid 1000's own `npm install -g` (of the same package too) works after it; local tarballs, so offline
check "sudo npm -g installs into /usr/local, then uid 1000's npm -g into ~/.npm-global" img_run -u 1000 "$img" -c '
  set -e
  cd "$(mktemp -d)"
  for p in smoke-a smoke-b; do
    mkdir $p && printf "{\"name\":\"%s\",\"version\":\"1.0.0\",\"bin\":{\"%s\":\"b.js\"}}" $p $p > $p/package.json
    printf "#!/usr/bin/env node\nconsole.log(1)\n" > $p/b.js
    (cd $p && npm pack -q >/dev/null)
  done
  test "$(sudo -n npm config get prefix)" = /usr/local
  sudo -n npm install -g -q --offline --no-audit --no-fund "$PWD/smoke-a/smoke-a-1.0.0.tgz" >/dev/null 2>&1
  test -d /usr/local/lib/node_modules/smoke-a
  test -z "$(find /home/sandbox -user root -print -quit)"
  for p in smoke-b smoke-a; do
    env -i PATH="$PATH" HOME=/home/sandbox LANG=C.UTF-8 npm install -g -q --offline --no-audit --no-fund "$PWD/$p/$p-1.0.0.tgz" >/dev/null 2>&1
    test -d /home/sandbox/.npm-global/lib/node_modules/$p
  done
  test "$(env -i PATH="$PATH" HOME=/home/sandbox sh -c "command -v smoke-b")" = /home/sandbox/.npm-global/bin/smoke-b'
# PYTHONUNBUFFERED's job without the environment: output to a pipe (BashOutput) is line-buffered
check "python3's stdout and stderr to a pipe are line-buffered with only PATH and HOME" img_run -u 1000 "$img" -c '
  env -i PATH="$PATH" HOME=/home/sandbox python3 -c "import sys; assert sys.stdout.line_buffering and sys.stderr.line_buffering" 2>&1 | cat
  test "${PIPESTATUS[0]}" = 0'
check "/opt/image-setup has the Dockerfile and the files it installs" img_run "$img" -c '
  cd /opt/image-setup && ls Dockerfile apt-wrapper.sh docker-wrapper.sh reportlab-cjk.py line-buffered.pth 90sandbox sudoers npmrc pip.conf profile >/dev/null'
check "uid 1000 can apt-get install sl (through sudo, fetching the lists first)" \
  docker run --rm --network "${SMOKE_INTERNET_NETWORK:-bridge}" --entrypoint /bin/bash -u 1000 "$img" -c 'apt-get install -qq sl >/dev/null 2>&1 && test -x /usr/games/sl'
code=0
docker run --rm --network none "${sbx_host[@]}" --security-opt no-new-privileges --entrypoint /bin/bash -u 1000 "$img" \
  -c 'apt-get install sl' > "$work/nnp.log" 2>&1 || code=$?
check "without root (no-new-privileges) apt-get install exits 100 ($code)" test "$code" = 100

for proxy in "" "http://proxy.invalid:3128"; do
  # detached and waited on with a bound, then removed: a keeper that kept running is stopped here, not left behind
  docker rm -f "$name-nocred" >/dev/null 2>&1
  docker run -d --name "$name-nocred" --network none ${proxy:+-e HTTP_PROXY="$proxy"} "$img" >/dev/null
  code=$(timeout 30 docker wait "$name-nocred" || echo "still running after 30 s")
  docker logs "$name-nocred" > "$work/nocred.log" 2>&1
  docker rm -f "$name-nocred" >/dev/null 2>&1
  check "the keeper without a proxy credential (HTTP_PROXY='$proxy') exits 78 saying why at level error ($code)" \
    sh -c "test '$code' = 78 && grep -q '\"level\":\"error\",\"svc\":\"supervisor-keeper\",\"msg\":\"no sandbox proxy credential\"' '$work/nocred.log'"
done

fi

echo "== the keeper and the supervisor against the test gateway${SMOKE_RUNTIME:+ (runtime $SMOKE_RUNTIME)}"
"$bin/mock-model" --listen 127.0.0.1:7200 > "$work/mock-model.log" 2>&1 &
# the descriptor's memoryBytes: 16 GiB, since under runc the guard (supervisor 5) measures an unsized container on
# a shared host, not a sandbox of hermit's size
"$bin/fuse-rig" world "$work/rig" http://127.0.0.1:7200 17179869184 > "$work/fuse-rig.log" 2>&1 &
for _ in $(seq 100); do [ -s "$work/rig/proxy" ] && break; sleep 0.1; done
if [ ! -s "$work/rig/proxy" ]; then fail "fuse-rig's world did not come up"; cat "$work/fuse-rig.log"; exit 1; fi
proxy=$(cat "$work/rig/proxy")
cid=$(cat "$work/rig/cid")
# --privileged: /dev/fuse and the capabilities the keeper (its tmpfs, oom_score_adj) and dockerd need, as a
# sandbox's root has them inside gVisor; /var/lib/docker on a volume (the host's filesystem), since dockerd's overlay
# snapshots cannot sit on the container's own overlay root under runc (in a sandbox they sit on its scratch disk)
docker run -d --name "$name" ${SMOKE_RUNTIME:+--runtime "$SMOKE_RUNTIME"} --network host --privileged -v /var/lib/docker \
  -e HTTP_PROXY="$proxy" -e HOME=/home/sandbox "$img" >/dev/null
x() { docker exec "$name" "$@"; }
mounted=
for _ in $(seq 120); do
  if x grep -q ' /home/sandbox/.claude fuse' /proc/mounts 2>/dev/null; then mounted=1; break; fi
  sleep 0.5
done
check "the supervisor mounted ~/.claude (FUSE) under the keeper" test -n "$mounted"
if [ -z "${SMOKE_RUNTIME:-}" ]; then
  check "the keeper is PID 1 (RSS $(x awk '/VmRSS/ {print $2 " " $3}' /proc/1/status), at most 4 MiB)" \
    x sh -c 'test "$(cat /proc/1/comm)" = supervisor-keep && test "$(awk "/VmRSS/ {print \$2}" /proc/1/status)" -le 4096'
else
  # gVisor's sentry accounts VmRSS its own way (the same keeper reads 9 MiB under runsc, under 4 MiB under runc):
  # the budget is checked under runc (sandbox-smoke); here PID 1's identity, and its RSS for the record
  check "the keeper is PID 1 (RSS under $SMOKE_RUNTIME $(x awk '/VmRSS/ {print $2 " " $3}' /proc/1/status))" \
    x sh -c 'test "$(cat /proc/1/comm)" = supervisor-keep'
fi
check "the container has no address for model or tools: only the proxy reaches them" \
  x sh -c '! getent hosts model && ! getent hosts tools'

# the turn's model replies: load the deferred MCP tool (Claude Code 2.1.304 defers MCP tools behind ToolSearch), call
# it, then (unscripted) "ok"; matched to Claude Code's model, not the title model the supervisor's title call uses
default_model=$(curl -sf http://127.0.0.1:7200/v1/aviary/models | jq -r .default)
# shellcheck disable=SC2317 # run by check
script_turn() {
  local entry
  for entry in \
    '{match: {conversation: $c, model: $m, textContains: "smoke-echo"},
      reply: [{toolUse: {name: "ToolSearch", input: {query: "select:mcp__tools__echo", max_results: 1}}}]}' \
    '{match: {conversation: $c, model: $m}, reply: [{toolUse: {name: "mcp__tools__echo", input: {text: "smoke-echo"}}}]}'
  do
    jq -nc --arg c "$cid" --arg m "$default_model" "$entry" |
      curl -sf -X POST -H 'content-type: application/json' --data @- http://127.0.0.1:7200/__script -o /dev/null || return 1
  done
}
check "mock-model took the turn's script (Claude Code's model: $default_model)" script_turn
turn=$(curl -sf -X POST -H 'content-type: application/json' --data '{"text":"Call the echo tool with smoke-echo."}' \
  http://127.0.0.1:7300/turn)
tid=$(jq -r .turnId <<< "$turn")
end=
for _ in $(seq 240); do
  end=$(curl -sf http://127.0.0.1:7300/calls/turn_state |
    jq -r --arg t "$tid" '[.[] | select(.params.turnId == $t) | .body.state] | map(select(. == "completed" or . == "failed" or . == "canceled")) | first // empty')
  [ -n "$end" ] && break
  sleep 0.5
done
check "a start_turn from magpie ran Claude Code to the end: ${end:-no end}" test "$end" = completed
x sh -c 'cat /var/lib/supervisor/run/*/stdout.ndjson' > "$work/stdout.ndjson" 2>/dev/null
claude_pid=$(x sh -c 'for p in /proc/[0-9]*; do
  if [ "$(tr "\0" " " < $p/cmdline 2>/dev/null | cut -d" " -f1)" = /opt/claude-code/bin/claude ]; then basename $p; fi
done' | head -1)
check "Claude Code runs as uid 1000 from /opt/claude-code/bin/claude (pid ${claude_pid:-none})" \
  x sh -c "test -n '$claude_pid' && test \"\$(awk '/^Uid:/ {print \$2}' /proc/$claude_pid/status)\" = 1000"
check "the managed files are root's, 0644" x sh -c '
  for f in CLAUDE.md managed-settings.json managed-mcp.json; do
    test "$(stat -c "%U %a" /etc/claude-code/$f)" = "root 644" || exit 1
  done'
curl -sf "http://127.0.0.1:7200/__requests?conversation=$cid" > "$work/requests.json"
curl -sf http://127.0.0.1:7300/gateway > "$work/gateway.json"
curl -sf http://127.0.0.1:7300/tools > "$work/tools.json"
echo "  the gateway saw (method, target, User-Agent, status):"
jq -r '.[] | "    \(.method) \(.target) \(.userAgent // "-") \(.status)"' "$work/gateway.json" | sort | uniq -c
echo "  fake-tools answered, in order (method, JSON-RPC method, Mcp-Session-Id sent, status, User-Agent):"
jq -r '.[] | "    \(.method) \(.rpc // "-") session=\(.session) \(.status) \(.userAgent // "-")"' "$work/tools.json"
# Claude Code's own calls, told apart from the supervisor's (`User-Agent: supervisor/<sha12>`, its title and model
# calls take the same proxy) by their User-Agent: absolute-form requests to the fixed names, answered 200
cc='(.userAgent // "") as $ua | ($ua != "") and ($ua | startswith("supervisor/") | not)'
check "the gateway proxied Claude Code's POST http://model/v1/messages (absolute form, not the supervisor's)" \
  jq -e "any(.[]; .method == \"POST\" and (.target | test(\"^http://model/v1/messages([?]|\$)\")) and .status == 200 and ($cc))" "$work/gateway.json"
check "the gateway proxied Claude Code's POST http://tools/mcp (absolute form)" \
  jq -e "[.[] | select(.method == \"POST\" and .target == \"http://tools/mcp\" and (.status | . >= 200 and . < 300) and ($cc))] | length >= 2" "$work/gateway.json"
check "fake-tools got Claude Code's initialize and tools/list on /mcp through the gateway" \
  jq -e "[.[] | select(.path == \"/mcp\" and ($cc)) | .rpc] | index(\"initialize\") != null and index(\"tools/list\") != null" "$work/tools.json"
# Claude Code 2.1.304 first probes `server/discover` without a session (400, as from the MCP SDK's Streamable-HTTP
# server, which needs `initialize` first), then initializes; a GET for a server stream is 405 (fake-tools offers none)
check "fake-tools' only non-2xx answers are the session-less server/discover probe (400) and the stream GET (405)" \
  jq -e "[.[] | select(.path == \"/mcp\" and ($cc) and (.status | . < 200 or . >= 300)) | \"\\(.method) \\(.rpc) \\(.status)\"]
    - [\"POST server/discover 400\", \"GET null 405\"] == []" "$work/tools.json"
check "Claude Code's tools/call reached fake-tools through the gateway with its session and was answered 200" \
  jq -e "any(.[]; .path == \"/mcp\" and .rpc == \"tools/call\" and .session and .status == 200 and ($cc))" "$work/tools.json"
check "the transcript has mcp__tools__echo's result, smoke-echo, not an error" \
  jq -se '([.[] | select(.type == "assistant") | .message.content[]? | select(.type == "tool_use" and .name == "mcp__tools__echo") | .id]) as $ids
    | ($ids | length) > 0 and any(.[] | select(.type == "user") | .message.content[]? | select(.type == "tool_result");
        (.tool_use_id as $t | $ids | index($t) != null) and (.is_error != true) and (tostring | contains("smoke-echo")))' \
  "$work/stdout.ndjson"
check "mock-model filed Claude Code's Messages calls under the conversation the gateway adds ($(jq length "$work/requests.json"))" \
  jq -e 'length > 0' "$work/requests.json"
check "Claude Code reported the MCP server tools connected, with fake-tools' tools as mcp__tools__*" \
  jq -se '[.[] | select(.type == "system" and .subtype == "init")] | first
    | (.mcp_servers | any(.name == "tools" and .status == "connected")) and (.tools | index("mcp__tools__echo") != null)' \
  "$work/stdout.ndjson"
check "the memory guard runs (the keeper is PID 1)" sh -c "! docker logs '$name' 2>&1 | grep -q 'memory guard off'"

# Docker on demand: the wrapper asks the supervisor (GET /start on /run/supervisor/docker.sock), which starts
# dockerd through the keeper; uid 1000 then uses it (dockerd's socket is group node's) and runs a container from an
# image imported offline (one static binary)
check "uid 1000's docker starts dockerd through /run/supervisor/docker.sock" \
  docker exec -u node "$name" bash -c 'test -S /run/supervisor/docker.sock && docker version --format "{{.Server.Version}}"'
check "and runs a container" docker exec -u node "$name" bash -c '
  d=$(mktemp -d) && cp /usr/local/bin/supervisor "$d/" &&
  tar -C "$d" -c . | docker import - smoke:1 >/dev/null && docker run --rm smoke:1 /supervisor --version | grep -q "^supervisor "'

# a runtime stop: SIGTERM to the keeper is a forced release, then everything ends and the keeper exits 0
docker stop -t 30 "$name" >/dev/null
code=$(docker inspect -f '{{.State.ExitCode}}' "$name")
check "a runtime stop (SIGTERM to PID 1) ends the sandbox with exit 0 ($code)" test "$code" = 0

if [ "$status" != 0 ]; then
  echo "== the sandbox's console"; docker logs "$name" 2>&1 | tail -n 80
  echo "== Claude Code's stdout"; tail -n 20 "$work/stdout.ndjson"
  echo "== mock-model"; tail -n 20 "$work/mock-model.log"
fi
exit "$status"

#!/bin/bash
# `docker` in the sandbox (installed as /usr/local/bin/docker, ahead of /usr/bin/docker on PATH): starts dockerd on the
# first use (sbx-init.mjs, sandboxes on our own hosts only), then runs the real CLI. Inside gVisor dockerd has no
# bridge and no iptables, so containers and builds use the sandbox's network (--network=host, added here unless the
# command names a network) and reach the internet only through the egress proxy, like everything else in the sandbox.
real=/usr/bin/docker
if [ -z "${DOCKER_HOST:-}" ] && ! curl -sf --max-time 2 --unix-socket /var/run/docker.sock http://d/_ping >/dev/null 2>&1; then
  if [ -S /run/sbx-docker/start.sock ]; then
    if ! out=$(curl -sS --max-time 90 --unix-socket /run/sbx-docker/start.sock http://d/start 2>&1) || [ "$out" != ok ]; then
      echo "docker: could not start the Docker daemon: $out" >&2; exit 1
    fi
  else
    echo "docker: Docker is not available in this sandbox" >&2; exit 1
  fi
fi
# containers and builds get the proxy settings (the CLI passes ~/.docker/config.json "proxies" into them)
cfg="${DOCKER_CONFIG:-$HOME/.docker}/config.json"
if [ -n "${HTTPS_PROXY:-}" ] && [ ! -e "$cfg" ]; then
  mkdir -p "$(dirname "$cfg")"
  printf '{"proxies":{"default":{"httpProxy":"%s","httpsProxy":"%s","noProxy":""}}}\n' "$HTTP_PROXY" "$HTTPS_PROXY" > "$cfg"
fi
args=("$@")
# find the subcommand (after global options) and add --network=host to run/create/build without a network
i=0
while [ $i -lt ${#args[@]} ]; do
  a=${args[$i]}
  case $a in
    --config|--context|-c|--host|-H|--log-level|-l|--tlscacert|--tlscert|--tlskey) i=$((i+2)); continue ;;
    -*) i=$((i+1)); continue ;;
  esac
  break
done
sub=${args[$i]:-}; at=$((i+1))
if [ "$sub" = container ] || [ "$sub" = image ] || [ "$sub" = buildx ]; then sub=${args[$at]:-}; at=$((at+1)); fi
case $sub in
  run|create|build)
    has=0
    for a in "${args[@]:$at}"; do case $a in --net|--net=*|--network|--network=*) has=1 ;; --) break ;; esac; done
    [ $has = 1 ] || args=("${args[@]:0:$at}" --network=host "${args[@]:$at}")
    ;;
esac
exec "$real" "${args[@]}"

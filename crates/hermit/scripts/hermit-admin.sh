#!/bin/bash
# hermit-admin.sh <verb> [args]: hermit's admin routes (hermit spec 16) for the operator and runbooks.
#
# Reach hermit first, e.g. `kubectl -n <namespace> port-forward svc/hermit 18080:80`. The admin pair token is read
# from HERMIT_ADMIN_TOKEN (never an argument, so it stays out of the shell history and the process list), the
# operator's email from HERMIT_ADMIN (recorded in admin_events), the base URL from HERMIT_URL
# (default http://127.0.0.1:18080).
#
# Verbs:
#   hosts                                   live hosts with slots, sandboxes, retire_by, cap
#   cap <host> <n|null>                     cap a host (0 = no new sandboxes, null = by memory/CPU)
#   retire <host> | reactivate <host>       retire a host (its sandboxes move off) or set it active again
#   disable <reason> | enable               the off switch for new sandboxes
#   fresh-host <minutes>                    only hosts that become active from now are placeable (1..60)
#   sandboxes [conversation=<id>] [person=<email>] [since=<RFC 3339>]
#   log <sandbox id>                        the stored console tail
#   release <sandbox id> <note>             forced release `operator`
#   exec <sandbox id> <argv…>               run a command in the sandbox (argv and exit code are audited)
#   recycle <sha256:…> [--force]            release every live sandbox on another image (soft unless --force)
#   purge <email>                           delete a person's data (refused while the catalogue allows them)
#   events [limit]                          the audit log
set -euo pipefail

: "${HERMIT_ADMIN_TOKEN:?set HERMIT_ADMIN_TOKEN to the admin pair token}"
: "${HERMIT_ADMIN:?set HERMIT_ADMIN to your email}"
URL=${HERMIT_URL:-http://127.0.0.1:18080}

call() { # method path [json body]
  local args=(-sS -X "$1" --fail-with-body -H "aviary-protocol: hermit-core=1" -H "aviary-admin: $HERMIT_ADMIN"
    -H "content-type: application/json" -K -)
  if [ $# -ge 3 ]; then args+=(--data "$3"); fi
  # the token goes to curl on stdin (a config line), never on its command line
  printf 'header = "authorization: Bearer %s"\n' "$HERMIT_ADMIN_TOKEN" | curl "${args[@]}" "$URL$2"
  echo
}

json() { # a JSON string of $1
  local s=${1//\\/\\\\}
  s=${s//\"/\\\"}
  printf '"%s"' "$s"
}

enc() { # percent-encode $1 for a path segment or a query value
  local s=$1 out="" c i
  for ((i = 0; i < ${#s}; i++)); do
    c=${s:i:1}
    case "$c" in
      [a-zA-Z0-9.~_-]) out+=$c ;;
      *) out+=$(printf '%%%02X' "'$c") ;;
    esac
  done
  printf '%s' "$out"
}

verb=${1:-}
shift || true
case "$verb" in
  hosts) call GET /v1/admin/hosts ;;
  cap) call PATCH "/v1/admin/hosts/$(enc "$1")" "{\"maxSandboxes\": $2}" ;;
  retire) call PATCH "/v1/admin/hosts/$(enc "$1")" '{"retire": true}' ;;
  reactivate) call PATCH "/v1/admin/hosts/$(enc "$1")" '{"reactivate": true}' ;;
  disable) call PUT /v1/admin/sandboxes-enabled "{\"enabled\": false, \"reason\": $(json "$*")}" ;;
  enable) call PUT /v1/admin/sandboxes-enabled '{"enabled": true}' ;;
  fresh-host) call POST /v1/admin/fresh-host "{\"ttlMinutes\": $1}" ;;
  sandboxes)
    q=""
    for kv in "$@"; do q+="${q:+&}${kv%%=*}=$(enc "${kv#*=}")"; done
    call GET "/v1/admin/sandboxes${q:+?$q}"
    ;;
  log) call GET "/v1/admin/sandboxes/$1/log" ;;
  release) id=$1; shift; call POST "/v1/admin/sandboxes/$id/release" "{\"note\": $(json "$*")}" ;;
  exec)
    id=$1; shift
    argv=""
    for a in "$@"; do argv+="${argv:+, }$(json "$a")"; done
    call POST "/v1/admin/sandboxes/$id/exec" "{\"argv\": [$argv]}"
    ;;
  recycle)
    idle=true
    if [ "${2:-}" = "--force" ]; then idle=false; fi
    call POST /v1/admin/recycle "{\"exceptImage\": $(json "$1"), \"onlyIdle\": $idle}"
    ;;
  purge) call POST "/v1/admin/people/$(enc "$1")/purge" "{\"confirm\": $(json "$1")}" ;;
  events) call GET "/v1/admin/events?limit=${1:-100}" ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 2 ;;
esac

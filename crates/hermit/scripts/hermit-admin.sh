#!/bin/bash
# hermit-admin.sh <verb> [args]: hermit's admin routes (hermit spec 16) for the operator and runbooks.
#
# Reach hermit first, e.g. `kubectl -n <namespace> port-forward svc/hermit 18080:80`. The admin pair token is read
# from HERMIT_ADMIN_TOKEN (never an argument, so it stays out of the shell history and the process list): the
# runbook puts the deployment's token there from wherever it keeps it (this public script names no store). The
# operator's email comes from HERMIT_ADMIN (recorded in admin_events), the base URL from HERMIT_URL
# (default http://127.0.0.1:18080). Needs curl and jq; every body is built by jq, so any text is valid JSON.
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
#   gateway-maintenance --node-work CMD [--deadline MIN] [--back MIN] [--settle S]
#                                           the gateway node runbook (hermit 14.7, fabric/host/gateway-maintenance.md):
#                                           (1) new sandboxes off ("gateway maintenance"); (2) wait until no sandbox
#                                           is busy, at most --deadline minutes (default 30), then go on anyway;
#                                           (3) run CMD with bash, which must block until the node work is done (a
#                                           failure is reported, the steps after still run); (4) wait until the
#                                           gateway answers (its host row reachable) with egress targets pushed, for
#                                           --settle seconds in a row (default 30), at most --back minutes (default
#                                           30); (5) new sandboxes on. If (4) times out, new sandboxes stay off and
#                                           the script exits 1 (`enable` once the gateway is back). Exit 0 only when
#                                           CMD succeeded and sandboxes are on again. HERMIT_POLL: seconds between
#                                           snapshot reads (default 5).
set -euo pipefail

: "${HERMIT_ADMIN_TOKEN:?set HERMIT_ADMIN_TOKEN to the admin pair token}"
: "${HERMIT_ADMIN:?set HERMIT_ADMIN to your email}"
URL=${HERMIT_URL:-http://127.0.0.1:18080}
# the token goes into a curl config line in double quotes: only a bearer token's characters (RFC 6750 b64token), so
# no quote, backslash or newline can end or bend that line
if ! [[ $HERMIT_ADMIN_TOKEN =~ ^[A-Za-z0-9._~+/-]+=*$ ]]; then
  echo "HERMIT_ADMIN_TOKEN is not a bearer token (RFC 6750 b64token characters only)" >&2
  exit 2
fi

call() { # method path [json body]
  local args=(-sS -X "$1" --fail-with-body -H "aviary-protocol: hermit-core=1" -H "aviary-admin: $HERMIT_ADMIN"
    -H "content-type: application/json" -K -)
  if [ $# -ge 3 ]; then args+=(--data "$3"); fi
  # the token goes to curl on stdin (a config line), never on its command line
  printf 'header = "authorization: Bearer %s"\n' "$HERMIT_ADMIN_TOKEN" | curl "${args[@]}" "$URL$2"
  echo
}

json() { # a JSON string of $1 (newlines, tabs, quotes and any Unicode escaped by jq)
  jq -cn --arg s "$1" '$s'
}

enc() { # percent-encode $1's UTF-8 bytes for a path segment or a query value
  jq -rn --arg s "$1" '$s | @uri'
}

verb=${1:-}
shift || true
case "$verb" in
  hosts) call GET /v1/admin/hosts ;;
  cap) call PATCH "/v1/admin/hosts/$(enc "$1")" "$(jq -cn --argjson n "$2" '{maxSandboxes: $n}')" ;;
  retire) call PATCH "/v1/admin/hosts/$(enc "$1")" '{"retire": true}' ;;
  reactivate) call PATCH "/v1/admin/hosts/$(enc "$1")" '{"reactivate": true}' ;;
  disable) call PUT /v1/admin/sandboxes-enabled "{\"enabled\": false, \"reason\": $(json "$*")}" ;;
  enable) call PUT /v1/admin/sandboxes-enabled '{"enabled": true}' ;;
  fresh-host) call POST /v1/admin/fresh-host "$(jq -cn --argjson n "$1" '{ttlMinutes: $n}')" ;;
  sandboxes)
    q=""
    for kv in "$@"; do q+="${q:+&}${kv%%=*}=$(enc "${kv#*=}")"; done
    call GET "/v1/admin/sandboxes${q:+?$q}"
    ;;
  log) call GET "/v1/admin/sandboxes/$1/log" ;;
  release) id=$1; shift; call POST "/v1/admin/sandboxes/$id/release" "{\"note\": $(json "$*")}" ;;
  exec)
    id=$1; shift
    call POST "/v1/admin/sandboxes/$id/exec" "$(jq -cn '{argv: $ARGS.positional}' --args "$@")"
    ;;
  recycle)
    idle=true
    if [ "${2:-}" = "--force" ]; then idle=false; fi
    call POST /v1/admin/recycle "{\"exceptImage\": $(json "$1"), \"onlyIdle\": $idle}"
    ;;
  purge) call POST "/v1/admin/people/$(enc "$1")/purge" "{\"confirm\": $(json "$1")}" ;;
  events) call GET "/v1/admin/events?limit=$(enc "${1:-100}")" ;;
  gateway-maintenance)
    work='' deadline=30 back=30 settle=30
    while [ $# -gt 0 ]; do
      case "$1" in
        --node-work) work=${2:?--node-work CMD}; shift 2 ;;
        --deadline) deadline=${2:?minutes}; shift 2 ;;
        --back) back=${2:?minutes}; shift 2 ;;
        --settle) settle=${2:?seconds}; shift 2 ;;
        *) echo "gateway-maintenance: unknown argument $1" >&2; exit 2 ;;
      esac
    done
    [ -n "$work" ] || { echo "gateway-maintenance needs --node-work CMD" >&2; exit 2; }
    for n in "$deadline" "$back" "$settle" "${HERMIT_POLL:-5}"; do
      [[ $n =~ ^[0-9]+$ ]] || { echo "gateway-maintenance: '$n' is not a whole number" >&2; exit 2; }
    done
    poll=${HERMIT_POLL:-5}
    log() { echo "gateway-maintenance: $*" >&2; }
    # the ops snapshot, or nothing when hermit does not answer (a poll goes on)
    snap() { call GET /ops/snapshot 2>/dev/null || true; }
    trap 'log "interrupted: new sandboxes may still be OFF; run \`hermit-admin.sh enable\` once the gateway is back"; exit 130' INT TERM
    # step 1
    call PUT /v1/admin/sandboxes-enabled '{"enabled": false, "reason": "gateway maintenance"}' >/dev/null
    log "1/5 new sandboxes off (sandbox_busy disabled)"
    # step 2: every sandbox not busy, as the snapshot's sandboxes section says (a failed section counts as busy)
    end=$((SECONDS + deadline * 60))
    while :; do
      busy=$(snap | jq -r 'if .sandboxes.list == null then "?" else [.sandboxes.list[] | select(.busy)] | length end' 2>/dev/null || echo "?")
      [ "$busy" = 0 ] && { log "2/5 no busy sandbox"; break; }
      [ "$SECONDS" -lt "$end" ] || { log "2/5 deadline of $deadline min reached with ${busy} busy sandbox(es): going on"; break; }
      sleep "$poll"
    done
    # step 3
    log "3/5 node work: $work"
    rc=0; bash -c "$work" || rc=$?
    if [ "$rc" = 0 ]; then log "3/5 node work done"; else log "3/5 node work FAILED (exit $rc)"; fi
    # step 4: the gateway's host row reachable and egress targets pushed, for $settle seconds in a row
    end=$((SECONDS + back * 60)) since=
    while :; do
      ok=$(snap | jq -r '([.hosts.list[]? | select(.gateway and .reachable)] | length > 0)
                         and ((.fabricOps.egressTargets // 0) > 0)' 2>/dev/null || echo false)
      if [ "$ok" = true ]; then
        since=${since:-$SECONDS}
        [ $((SECONDS - since)) -lt "$settle" ] || { log "4/5 gateway answering with egress targets for ${settle}s"; break; }
      else
        since=
      fi
      if [ "$SECONDS" -ge "$end" ]; then
        log "4/5 the gateway has not answered with egress targets within $back min: new sandboxes stay OFF; run \`hermit-admin.sh enable\` once it is back"
        exit 1
      fi
      sleep "$poll"
    done
    # step 5
    call PUT /v1/admin/sandboxes-enabled '{"enabled": true}' >/dev/null
    log "5/5 new sandboxes on"
    exit "$rc"
    ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d'; exit 2 ;;
esac

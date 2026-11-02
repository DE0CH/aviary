#!/usr/bin/env bash
# Supervisor 22 item 4 on the real build: the fault-injection build of the real supervisor (RIG_FI=1) under the real
# keeper SIGKILLs itself at each journal point (read-returned, call-sent, call-answered, reply-written) of a WRITE,
# a MKDIR, a CREATE (O_EXCL) and a RENAME; the keeper restarts it, the new supervisor takes the /dev/fuse fd back
# (GET_FD), replays the journal, and the caller's syscall completes. Pass per case: the syscall returns success once,
# magpie applied the operation under one opId and holds the expected bytes or names, the supervisor died at that
# point and the next one replayed the request.
# Usage: rig-kill.sh NAME   (LAYOUT=scratch by default: the journal on the host-backed scratch dir, as in production)
# Outputs: /root/out/kill-NAME/ (cases.jsonl, one line per case; supervisor.log; calls-*.json; nodes.txt)
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
name=$1; layout=${LAYOUT:-scratch}
out=/root/out/kill-$name; rm -rf "$out"; mkdir -p "$out"
./gate.sh rm "$name"; rm -rf "/var/lib/gate/scratch/$name"; : > "/var/log/gate/$name.log"
RIG=1 RIG_FI=1 LAYOUT=$layout ./gate.sh bundle "$name"; ./gate.sh run "$name"
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
ctl() { r python3 /media/rig/rigctl.py "$@"; }
ctl wait-mount 30 || { echo "FAIL: no mount"; exit 1; }
fails=0
i=0
for op in write mkdir create rename; do
  case $op in
    write) route=cc_write; opc=WRITE ;; mkdir) route=cc_mkdir; opc=MKDIR ;;
    create) route=cc_write; opc=CREATE ;; rename) route=cc_rename; opc=RENAME ;;
  esac
  for point in read-returned call-sent call-answered reply-written; do
    i=$((i + 1)); d=projects/k$i; P=/home/sandbox/.claude/$d
    x sh -c "mkdir -p $P && printf 'start\n' > $P/w.jsonl && printf 'src\n' > $P/r-src.json"
    before=$(ctl get "/calls/$route" | jq length)
    kills0=$(r grep -c 'fault-injection: SIGKILL' /srv/log/supervisor.log)
    # arm the point, then restart the supervisor so the new one reads /run/fault-injection
    r sh -c "echo '$point:$opc:1' > /run/fault-injection"
    ctl kill-supervisor > /dev/null
    for _ in $(seq 200); do r test -e /run/fault-injection || break; sleep 0.1; done
    ctl wait-mount 30
    case $op in
      write) res=$(timeout 120 ./gate.sh exec "$name" python3 /media/rig/rigop.py write "$P/w.jsonl" 1048576) ;;
      mkdir) res=$(timeout 120 ./gate.sh exec "$name" python3 /media/rig/rigop.py mkdir "$P/newdir") ;;
      create) res=$(timeout 120 ./gate.sh exec "$name" python3 /media/rig/rigop.py create "$P/new.jsonl") ;;
      rename) res=$(timeout 120 ./gate.sh exec "$name" python3 /media/rig/rigop.py rename "$P/r-src.json" "$P/r-dst.json") ;;
    esac
    sleep 0.5
    ctl get "/calls/$route" > "$out/calls-$i.json"
    nops=$(jq -r --argjson k "$before" '[.[$k:][] | (.query.opId // .body.opId)] | unique | length' "$out/calls-$i.json")
    kills=$(( $(r grep -c 'fault-injection: SIGKILL' /srv/log/supervisor.log) - kills0 ))
    nd() { ctl get "/node?path=$d/$1" || echo absent; }
    case $op in
      write) want=$(python3 -c "import hashlib; n=1048576; l=b'x'*127+b'\\n'; print(hashlib.sha256(b'start\\n'+(l*(n//len(l)+1))[:n-1]+b'\\n').hexdigest())")
             st=$(nd w.jsonl | jq --arg w "$want" '.sha256 == $w and .size == 1048582') ;;
      mkdir) st=$(nd newdir | jq '.kind == "Dir"' 2>/dev/null || echo false) ;;
      create) st=$(nd new.jsonl | jq '.size == 0' 2>/dev/null || echo false) ;;
      rename) st=$( [ "$(nd r-src.json)" = absent ] && nd r-dst.json | jq '.size == 4' 2>/dev/null || echo false) ;;
    esac
    ok=$(echo "$res" | jq -r .ok 2>/dev/null)
    pass=true; [ "$ok" = true ] && [ "$nops" = 1 ] && [ "$st" = true ] && [ "$kills" = 1 ] || pass=false
    [ $pass = true ] || fails=$((fails + 1))
    jq -cn --arg op "$opc" --arg point "$point" --arg ok "$ok" --argjson nops "${nops:-0}" --arg state "$st" \
      --argjson kills "$kills" --argjson pass $pass --arg res "$res" \
      '{op: $op, point: $point, syscall_ok: $ok, magpie_opids: $nops, magpie_state_ok: $state, killed_at_point: $kills, pass: $pass, result: $res}' \
      | tee -a "$out/cases.jsonl"
  done
done
r cat /srv/log/supervisor.log > "$out/supervisor.log"
r sh -c 'cat /srv/log/exits.log 2>/dev/null' > "$out/exits.log"
r python3 /media/rig/rigctl.py get /paths > "$out/paths.json"
grep -c '"journal replay"' "$out/supervisor.log" > "$out/replays.txt"
grep -c '"fuse connection"' "$out/supervisor.log" > "$out/connections.txt"
grep -E 'panicked|short read' "$out/supervisor.log" > "$out/supervisor-faults.txt" && fails=$((fails + 1))
./gate.sh rm "$name"
echo "cases: $(wc -l < "$out/cases.jsonl"), failed: $fails, replays logged: $(cat "$out/replays.txt"), fuse connections: $(cat "$out/connections.txt")"
[ "$fails" = 0 ] && echo PASS || { echo FAIL; exit 1; }

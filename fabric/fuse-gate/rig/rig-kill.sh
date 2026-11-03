#!/usr/bin/env bash
# Supervisor 22 item 4 on the real build: the fault-injection build of the real supervisor (RIG_FI=1) under the real
# keeper SIGKILLs itself at each journal point (read-returned, call-sent, call-answered, reply-written) of a WRITE,
# a MKDIR, a CREATE (O_EXCL) and a RENAME, and at read-returned and reply-written of a READ (a READ makes no
# state-changing call); the keeper restarts it, the new supervisor takes the /dev/fuse fd back (GET_FD), replays the
# journal, and the caller's syscall completes. Then a plain SIGKILL (no fault point) while a 1 MiB O_APPEND write is
# held in magpie. Pass per case: the syscall returns success once (a READ: the file's bytes), magpie applied the
# operation under one opId and holds the expected bytes or names, the supervisor died once and the next one replayed
# the request. Pass overall: `rig-uniques.py` finds no FUSE unique read twice across the restarts and no opId naming two
# operations.
# Usage: [OPS="write … plain"] rig-kill.sh NAME   (OPS: a subset of write mkdir create rename read plain; LAYOUT=scratch by default: the journal on the host-backed scratch dir, as in production)
# Outputs: /root/out/kill-NAME/ (cases.jsonl, one line per case; supervisor.log; calls-*.json; uniques.json)
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
for op in ${OPS:-write mkdir create rename read plain}; do
  case $op in
    write) route=cc_write; opc=WRITE ;; mkdir) route=cc_mkdir; opc=MKDIR ;;
    create) route=cc_write; opc=CREATE ;; rename) route=cc_rename; opc=RENAME ;;
    read) route=cc_file; opc=READ ;; plain) route=cc_write; opc=WRITE ;;
  esac
  points="read-returned call-sent call-answered reply-written"
  [ "$op" = read ] && points="read-returned reply-written"
  [ "$op" = plain ] && points="sigkill"
  for point in $points; do
    i=$((i + 1)); d=projects/k$i; P=/home/sandbox/.claude/$d
    x sh -c "mkdir -p $P && printf 'start\n' > $P/w.jsonl && printf 'src\n' > $P/r-src.json"
    [ "$op" = read ] && x python3 -c "import sys; l=b'r'*127+b'\\n'; open(sys.argv[1],'wb').write(l*8192)" "$P/rd.json"
    before=$(ctl get "/calls/$route" | jq length)
    kills0=$(r grep -c 'fault-injection: SIGKILL' /srv/log/supervisor.log)
    conns0=$(r grep -c '"fuse connection"' /srv/log/supervisor.log)
    replays0=$(r grep -c '"journal replay"' /srv/log/supervisor.log)
    if [ "$op" = plain ]; then
      # a plain SIGKILL while the write is held in magpie: no fault point, the replay re-sends it under its opId
      ctl post "/hold/$route" > /dev/null
      timeout 120 ./gate.sh exec "$name" python3 /media/rig/rigop.py write "$P/w.jsonl" 1048576 > "$out/plain-$i.json" &
      bg=$!
      ctl wait-held 1 15 > /dev/null || echo "case $i: the write never reached magpie's hold"
      ctl kill-supervisor > /dev/null
      # the new supervisor replays the held write before it serves any new request (9.9), so the mount answers only
      # once the hold is released: wait for its connection in the log, not for the mount
      for _ in $(seq 300); do
        [ "$(r grep -c '"fuse connection"' /srv/log/supervisor.log)" -gt "$conns0" ] && break; sleep 0.1
      done
      sleep 1
      ctl post /release > /dev/null
      ctl wait-mount 30
      wait $bg
      res=$(cat "$out/plain-$i.json")
    else
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
      read) res=$(timeout 120 ./gate.sh exec "$name" python3 /media/rig/rigop.py read "$P/rd.json") ;;
    esac
    fi
    sleep 0.5
    ctl get "/calls/$route" > "$out/calls-$i.json"
    nops=$(jq -r --argjson k "$before" '[.[$k:][] | (.query.opId // .body.opId)] | unique | length' "$out/calls-$i.json")
    kills=$(( $(r grep -c 'fault-injection: SIGKILL' /srv/log/supervisor.log) - kills0 ))
    # a READ changes nothing in magpie (no opId); the plain kill counts when exactly one new supervisor took the
    # connection and replayed the held write
    [ "$op" = read ] && nops=1
    if [ "$op" = plain ]; then
      conns=$(( $(r grep -c '"fuse connection"' /srv/log/supervisor.log) - conns0 ))
      replays=$(( $(r grep -c '"journal replay"' /srv/log/supervisor.log) - replays0 ))
      kills=0; [ "$conns" = 1 ] && [ "$replays" -ge 1 ] && kills=1
    fi
    nd() { ctl get "/node?path=$d/$1" || echo absent; }
    case $op in
      write|plain) want=$(python3 -c "import hashlib; n=1048576; l=b'x'*127+b'\\n'; print(hashlib.sha256(b'start\\n'+(l*(n//len(l)+1))[:n-1]+b'\\n').hexdigest())")
             st=$(nd w.jsonl | jq --arg w "$want" '.sha256 == $w and .size == 1048582') ;;
      mkdir) st=$(nd newdir | jq '.kind == "Dir"' 2>/dev/null || echo false) ;;
      create) st=$(nd new.jsonl | jq '.size == 0' 2>/dev/null || echo false) ;;
      rename) st=$( [ "$(nd r-src.json)" = absent ] && nd r-dst.json | jq '.size == 4' 2>/dev/null || echo false) ;;
      read) st=$(echo "$res" | jq --arg w "$(nd rd.json | jq -r .sha256)" '.sha256 == $w and .size == 1048576') ;;
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
args=()
for rt in cc_write cc_mkdir cc_rename cc_truncate cc_setattr cc_unlink cc_rmdir; do
  ctl get "/calls/$rt" > "$out/calls-final-$rt.json" && args+=("$rt=$out/calls-final-$rt.json")
done
python3 rig/rig-uniques.py "$out/supervisor.log" "${args[@]}" > "$out/uniques.json" || fails=$((fails + 1))
r sh -c 'cat /srv/log/exits.log 2>/dev/null' > "$out/exits.log"
r python3 /media/rig/rigctl.py get /paths > "$out/paths.json"
grep -c '"journal replay"' "$out/supervisor.log" > "$out/replays.txt"
grep -c '"fuse connection"' "$out/supervisor.log" > "$out/connections.txt"
grep -E 'panicked|short read' "$out/supervisor.log" > "$out/supervisor-faults.txt" && fails=$((fails + 1))
./gate.sh rm "$name"
echo "cases: $(wc -l < "$out/cases.jsonl"), failed: $fails, replays logged: $(cat "$out/replays.txt"), fuse connections: $(cat "$out/connections.txt"), uniques: $(cat "$out/uniques.json")"
if [ "$fails" = 0 ]; then echo PASS; else echo FAIL; exit 1; fi

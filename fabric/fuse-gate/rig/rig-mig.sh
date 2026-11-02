#!/usr/bin/env bash
# WP-22's migration acceptance on the real build (supervisor 9.3's resend rule, 22 item 3): a sandbox running the
# real supervisor-keeper and supervisor (gate.sh RIG=1) is live-migrated while one operation on ~/.claude is in
# flight, held by fake magpie (fuse-rig's world). OP is one of
#   write   a 1 MiB O_APPEND write(2) to an existing transcript (cc_write held)
#   create  open(O_CREAT|O_EXCL) of a new file (cc_write held: a create is a cc_write)
#   mkdir   mkdir(2) (cc_mkdir held)
#   rename  rename(2) over nothing (cc_rename held)
# (one per run: a held namespace operation holds the runtime's filesystem-wide lock, so two cannot be in flight at
# once). The hold is released on the target after the restore. Pass: the caller's syscall returns the original's
# success exactly once, magpie applied the operation under one opId (every call of the route after the setup carries
# the same opId) and holds exactly the expected bytes or names, no supervisor restart, no panic, no process exit.
# Usage: rig-mig.sh NAME OP TARGET_IP    (LOOPBACK=1: TARGET_IP is ignored, the target is side b of this host)
# LAYOUT=tmpfs (default) or scratch (the supervisor's state, cache and journal on the host-backed scratch dir, moved
# by rsync rounds and the checksummed final delta behind sandboxd's relay, as sandboxd moves scratch).
# Every output lands in /root/out/rig-NAME.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
name=$1; op=$2; dst=${3:-::1}; layout=${LAYOUT:-tmpfs}
out=/root/out/rig-$name; rm -rf "$out"; mkdir -p "$out"
if [ -n "${LOOPBACK:-}" ]; then
  dst=::1
  T() { (export GATE_SIDE=b; bash -c "cd /root/gate && $*"); }
  tside=b
else
  SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i /root/.ssh/gate "root@$dst")
  T() { "${SSH[@]}" "cd /root/gate && $*"; }
  tside=
fi
ms() { date +%s%3N; }
fail=()
case $op in
  write) route=cc_write ;;
  create) route=cc_write ;;
  mkdir) route=cc_mkdir ;;
  rename) route=cc_rename ;;
  *) echo "unknown op $op" >&2; exit 2 ;;
esac
./gate.sh rm "$name"; rm -rf "/var/lib/gate/scratch/$name"; : > "/var/log/gate/$name.log"
RIG=1 LAYOUT=$layout ./gate.sh bundle "$name"; ./gate.sh run "$name"
T "./gate.sh rm $name; rm -rf /var/lib/gate${tside:+/$tside}/scratch/$name; mkdir -p /var/log/gate; : > /var/log/gate/${tside:+$tside-}$name.log; RIG=1 LAYOUT=$layout ./gate.sh bundle $name"
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
yr() { T "./gate.sh exec $name --root $*"; }
r python3 /media/rig/rigctl.py wait-mount 30 || fail+=("the supervisor never mounted ~/.claude")
P=/home/sandbox/.claude/projects/p
x sh -c "mkdir -p $P && printf 'start\n' > $P/w.jsonl && printf 'src\n' > $P/r-src.json && ls -la $P" > "$out/setup.txt" 2>&1
before=$(r python3 /media/rig/rigctl.py get "/calls/$route" | jq length)
case $op in
  write) cmd="python3 /media/rig/rigop.py write $P/w.jsonl 1048576" ;;
  create) cmd="python3 /media/rig/rigop.py create $P/new.jsonl" ;;
  mkdir) cmd="python3 /media/rig/rigop.py mkdir $P/newdir" ;;
  rename) cmd="python3 /media/rig/rigop.py rename $P/r-src.json $P/r-dst.json" ;;
esac
r python3 /media/rig/rigctl.py post "/hold/$route"
r sh -c "echo '$cmd > /tmp/op.json' > /run/jobs/op.tmp && mv /run/jobs/op.tmp /run/jobs/op.sh"
r python3 /media/rig/rigctl.py wait-held 1 15 > "$out/held-before.txt" || fail+=("the operation never reached magpie's hold")
sleep 1
if [ "$layout" = scratch ]; then
  T "./gate.sh receive-relay $name 7000" > "$out/receive.json" &
else
  T "./gate.sh receive $name 7000" > "$out/receive.json" &
fi
rj=$!
for _ in $(seq 100); do T "ss -ltn | grep -q ':7000 '" && break; sleep 0.1; done
t0=$(ms)
if [ "$layout" = scratch ]; then
  TARGET_SIDE=$tside ./gate.sh send-scratch "$name" "$dst" 7000 > "$out/send.json" 2> "$out/send.txt"
else
  ./gate.sh send "$name" "$dst" 7000 > "$out/send.txt" 2>&1
fi
t1=$(ms)
wait $rj
t2=$(ms)
./gate.sh rm "$name"
yr python3 /media/rig/rigctl.py get /held > "$out/held-after-move.txt"
sleep 2
yr python3 /media/rig/rigctl.py post /release
yr sh -c "'for i in \$(seq 600); do [ -s /tmp/op.json ] && break; sleep 0.1; done; cat /tmp/op.json'" > "$out/op.json"
t3=$(ms)
sleep 1
yr python3 /media/rig/rigctl.py get "/calls/$route" > "$out/calls.json"
for p in w.jsonl new.jsonl newdir r-src.json r-dst.json; do
  echo "$p $(yr python3 /media/rig/rigctl.py get "'/node?path=projects/p/$p'" || echo absent)"
done > "$out/nodes.txt"
yr python3 /media/rig/rigctl.py get /paths > "$out/paths.json"
yr sh -c "'ls -la $P; cat $P/w.jsonl | sha256sum; wc -c < $P/w.jsonl'" > "$out/view.txt" 2>&1
yr cat /srv/log/supervisor.log > "$out/supervisor.log"
yr cat /srv/log/world.log > "$out/world.log"
yr sh -c "'cat /srv/log/exits.log 2>/dev/null'" > "$out/exits.log"
cp "/var/log/gate/$name.log" "$out/runsc-source.log"
T "cat /var/log/gate/${tside:+$tside-}$name.log" > "$out/runsc-target.log"
T "cat /var/log/gate/${tside:+$tside-}$name.restore.out" > "$out/runsc-restore.out"
[ "$layout" = scratch ] && T "cat /var/log/gate/${tside:+$tside-}$name.relay.log /var/log/gate/${tside:+$tside-}$name.rsync.log 2>/dev/null" > "$out/relay.log"
echo "{\"op\":\"$op\",\"layout\":\"$layout\",\"loopback\":\"${LOOPBACK:-}\",\"send_ms\":$((t1-t0)),\"total_ms\":$((t2-t0)),\"release_to_result_ms\":$((t3-t2))}" > "$out/times.json"
# checks
jq -e '.ok == true' "$out/op.json" >/dev/null 2>&1 || fail+=("the syscall did not return the original's success: $(tr -d '\n' < "$out/op.json")")
ops=$(jq -r --argjson k "$before" '.[$k:][] | (.query.opId // .body.opId)' "$out/calls.json" | sort -u)
nops=$(echo "$ops" | grep -c .)
echo "$ops" > "$out/opids.txt"
[ "$nops" = 1 ] || fail+=("magpie saw $nops opIds for the held operation (one expected)")
node() { grep "^$1 " "$out/nodes.txt" | cut -d' ' -f2-; }
case $op in
  write)
    want=$(python3 -c "import hashlib; n=1048576; l=b'x'*127+b'\\n'; print(hashlib.sha256(b'start\\n'+(l*(n//len(l)+1))[:n-1]+b'\\n').hexdigest())")
    node w.jsonl | jq -e --arg w "$want" '.sha256 == $w and .size == 1048582' >/dev/null || fail+=("magpie's transcript is not start + the 1 MiB once: $(node w.jsonl)") ;;
  create) node new.jsonl | jq -e '.size == 0' >/dev/null 2>&1 || fail+=("the created file is not in magpie: $(node new.jsonl)") ;;
  mkdir) node newdir | jq -e '.kind == "Dir"' >/dev/null 2>&1 || fail+=("the directory is not in magpie: $(node newdir)") ;;
  rename)
    node r-dst.json | jq -e '.size == 4' >/dev/null 2>&1 || fail+=("the rename's destination is not in magpie: $(node r-dst.json)")
    [ "$(node r-src.json)" = absent ] || fail+=("the rename's source is still in magpie") ;;
esac
grep -c '"fuse connection"' "$out/supervisor.log" > "$out/connections.txt"
[ "$(grep -c 'fuse connection' "$out/supervisor.log")" = 1 ] || fail+=("the supervisor restarted ($(grep -c 'fuse connection' "$out/supervisor.log") fuse connections)")
grep -E 'panicked|short read|SHORTREAD' "$out/supervisor.log" > "$out/supervisor-faults.txt" && fail+=("the supervisor logged a fault: $(head -n 3 "$out/supervisor-faults.txt" | tr '\n' ';')")
[ -s "$out/exits.log" ] && fail+=("an in-sandbox process exited: $(tr '\n' ';' < "$out/exits.log")")
[ "$layout" = scratch ] && { d=$(jq -r '.files_differing_after_final // ""' "$out/send.json" 2>/dev/null); [ -n "$d" ] && fail+=("scratch files differ after the final delta: $d"); }
grep -E 'resend|reply refused|journal replay' "$out/supervisor.log" > "$out/resend.txt"
printf '%s\n' "${fail[@]}" > "$out/failures.txt"
cat "$out/times.json"; cat "$out/op.json"; echo "opIds: $nops"; echo "resend lines: $(wc -l < "$out/resend.txt")"
T "./gate.sh rm $name; rm -rf /run/gate/ckpt/in-${tside:+$tside-}$name"
if [ "${#fail[@]}" -gt 0 ]; then echo "FAIL: $(tr '\n' ';' < "$out/failures.txt")"; exit 1; fi
echo PASS

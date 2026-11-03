#!/usr/bin/env bash
# Supervisor 22 item 3 on the real build with Claude Code mid-turn: Claude Code launched by the real supervisor
# (6.1-6.4) streams an 8 s answer (80 chunks 100 ms apart, the model is the gate's mock inside the sandbox) while
# fake magpie holds every cc write, so a direct 1 MiB O_APPEND to a transcript and Claude Code's own transcript appends
# are blocked inside the supervisor; the sandbox is live-migrated (scratch layout, sandboxd's way: rsync rounds,
# checkpoint --migrate-to, the checksummed final delta, the relay), the hold released on the target. FILL=1 first
# fills the content cache through the mount (64 x 4 MiB files and an 87 MiB file, 343 MiB) for the 2x rule.
# Pass: the turn ends completed on the target, the blocked write returns its full count once and is stored once,
# every magpie-scope file under ~/.claude equals magpie's copy, Claude Code's stdout continues after the move (the
# cc-tap log), one supervisor connection, no panic, no in-sandbox process exit.
# Usage: rig-migcc.sh NAME TARGET_IP   (LOOPBACK=1: the target is side b of this host). Outputs: /root/out/rmig-NAME/
set -uo pipefail
cd "$(dirname "$0")/.." || exit 1
name=$1; dst=${2:-::1}; out=/root/out/rmig-$name; rm -rf "$out"; mkdir -p "$out"
if [ -n "${LOOPBACK:-}" ]; then
  dst=::1; tside=b
  T() { (export GATE_SIDE=b; bash -c "cd /root/gate && $*"); }
else
  tside=
  SSH=(ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -i /root/.ssh/gate "root@$dst")
  T() { "${SSH[@]}" "cd /root/gate && $*"; }
fi
ms() { date +%s%3N; }
fail=()
env=(RIG=1 RIG_CC_TAP=1 LAYOUT=scratch GATE_MEM=$((12 << 30)) RIG_SANDBOX_MEMORY=$((10 << 30)))
./gate.sh rm "$name"; rm -rf "/var/lib/gate/scratch/$name"; mkdir -p /var/log/gate; : > "/var/log/gate/$name.log"
env "${env[@]}" ./gate.sh bundle "$name"; ./gate.sh run "$name"
T "./gate.sh rm $name; rm -rf /var/lib/gate${tside:+/$tside}/scratch/$name; mkdir -p /var/log/gate; : > /var/log/gate/${tside:+$tside-}$name.log; ${env[*]} ./gate.sh bundle $name"
x() { ./gate.sh exec "$name" "$@"; }
r() { ./gate.sh exec "$name" --root "$@"; }
yr() { T "./gate.sh exec $name --root $*"; }
r python3 /media/rig/rigctl.py wait-mount 60 || fail+=("the supervisor never mounted ~/.claude")
r sh -c "mkdir -p /tmp/s && python3 /media/rig/mksessions.py /tmp/s"
r python3 /media/rig/rigsession.py /tmp/s/r0.json /tmp/out-r0 > "$out/seed.summary.json"
P=/home/sandbox/.claude/projects/p
x sh -c "mkdir -p $P && printf 'start\n' > $P/w.jsonl"
if [ -n "${FILL:-}" ]; then
  x python3 -c "
import os
d='/home/sandbox/.claude/projects/fill'; os.makedirs(d, exist_ok=True)
for i in range(64):
    open(f'{d}/t{i:02d}.json','wb').write(os.urandom(2 << 20).hex().encode()[:4 << 20])
fd=os.open(f'{d}/big87.jsonl', os.O_WRONLY|os.O_CREAT|os.O_TRUNC, 0o600)
for i in range(87): os.write(fd, b'z'*((1<<20)-1)+b'\n')
os.close(fd)" || fail+=("the cache fill failed")
  r sh -c "du -sb /var/lib/supervisor/cache" > "$out/cache-before.txt"
fi
# the turn, as a job inside the sandbox (it migrates with it)
printf '%s' '{"resume": true, "turns": ["Migration turn.\nGATE: text 80 100"]}' > "$out/turn.json"
r sh -c "cat > /tmp/s/mig.json" < "$out/turn.json"
r sh -c "echo 'python3 /media/rig/rigsession.py /tmp/s/mig.json /tmp/mig-turn > /tmp/mig-turn.out' > /run/jobs/turn.tmp && mv /run/jobs/turn.tmp /run/jobs/turn.sh"
sleep 2
r python3 /media/rig/rigctl.py post /hold/cc_write
r sh -c "echo 'python3 /media/rig/rigop.py write $P/w.jsonl 1048576 > /tmp/op.json' > /run/jobs/op.tmp && mv /run/jobs/op.tmp /run/jobs/op.sh"
r python3 /media/rig/rigctl.py wait-held 1 15 > "$out/held-before.txt" || fail+=("the write never reached magpie's hold")
sleep 1
T "./gate.sh receive-relay $name 7000" > "$out/receive.json" &
rj=$!
for _ in $(seq 100); do T "ss -ltn | grep -q ':7000 '" && break; sleep 0.1; done
t0=$(ms)
TARGET_SIDE=$tside ./gate.sh send-scratch "$name" "$dst" 7000 > "$out/send.json" 2> "$out/send.txt"
t1=$(ms)
wait $rj
t2=$(ms)
./gate.sh rm "$name"
yr python3 /media/rig/rigctl.py get /held > "$out/held-after-move.txt"
sleep 2
yr python3 /media/rig/rigctl.py post /release
yr sh -c "'for i in \$(seq 1200); do [ -s /tmp/mig-turn/summary.json ] && [ -s /tmp/op.json ] && break; sleep 0.1; done; cat /tmp/op.json'" > "$out/op.json"
t3=$(ms)
yr cat /tmp/mig-turn/summary.json > "$out/turn.summary.json"
sleep 1
yr python3 /media/rig/rigreport.py > "$out/report.json"
yr python3 /media/rig/rigctl.py get /calls/cc_write > "$out/calls-cc_write.json"
yr python3 /media/rig/rigctl.py get "'/node?path=projects/p/w.jsonl'" > "$out/w.node.json"
yr cat /srv/log/supervisor.log > "$out/supervisor.log"
yr sh -c "'cat /srv/log/exits.log 2>/dev/null'" > "$out/exits.log"
yr cat /tmp/cc-tap/stdout.jsonl > "$out/cc-stdout.jsonl"
yr cat /srv/model/chunks.log > "$out/model.log"
cp "/var/log/gate/$name.log" "$out/runsc-source.log"
T "cat /var/log/gate/${tside:+$tside-}$name.log" > "$out/runsc-target.log"
T "cat /var/log/gate/${tside:+$tside-}$name.restore.out" > "$out/runsc-restore.out"
echo "{\"fill\":\"${FILL:-}\",\"loopback\":\"${LOOPBACK:-}\",\"send_ms\":$((t1-t0)),\"total_ms\":$((t2-t0)),\"release_to_done_ms\":$((t3-t2)),\"move_at_us\":$((t0 * 1000))}" > "$out/times.json"
# checks
jq -e '.ok == true and .ret == 1048576' "$out/op.json" >/dev/null 2>&1 || fail+=("the blocked write did not return its full count: $(tr -d '\n' < "$out/op.json")")
jq -e '.turns[0].state == "completed"' "$out/turn.summary.json" >/dev/null 2>&1 || fail+=("the turn did not complete: $(jq -c '.turns[0] | {state, error}' "$out/turn.summary.json" 2>/dev/null)")
want=$(python3 -c "import hashlib; n=1048576; l=b'x'*127+b'\\n'; print(hashlib.sha256(b'start\\n'+(l*(n//len(l)+1))[:n-1]+b'\\n').hexdigest())")
jq -e --arg w "$want" '.sha256 == $w and .size == 1048582' "$out/w.node.json" >/dev/null || fail+=("magpie's copy is not start + the 1 MiB once: $(cat "$out/w.node.json")")
jq -e '.files | length > 0' "$out/report.json" >/dev/null 2>&1 || fail+=("no file report (rigreport.py failed): magpie's copies were not compared")
bad=$(jq -r '[.files[] | select(.scope == "magpie" and .equal_to_magpie != true) | .path] | join(",")' "$out/report.json")
[ -z "$bad" ] || fail+=("files differ from magpie's copy: $bad")
after=$(jq -r --argjson t "$((t0 * 1000))" '[.[] | select(.t > $t)] | length' <(jq -s . "$out/cc-stdout.jsonl"))
echo "$after" > "$out/stdout-lines-after-move.txt"
[ "$after" -gt 0 ] || fail+=("no Claude Code stdout after the move")
[ "$(grep -c '"fuse connection"' "$out/supervisor.log")" = 1 ] || fail+=("the supervisor restarted")
grep -E 'panicked|short read|SHORTREAD' "$out/supervisor.log" > "$out/supervisor-faults.txt" && fail+=("the supervisor logged a fault: $(head -n 3 "$out/supervisor-faults.txt" | tr '\n' ';')")
[ -s "$out/exits.log" ] && fail+=("an in-sandbox process exited: $(tr '\n' ';' < "$out/exits.log")")
d=$(jq -r '.files_differing_after_final // ""' "$out/send.json" 2>/dev/null); [ -n "$d" ] && fail+=("scratch files differ after the final delta: $d")
grep -E 'resend|reply refused|journal replay' "$out/supervisor.log" > "$out/resend.txt"
printf '%s\n' "${fail[@]}" > "$out/failures.txt"
cat "$out/times.json"; jq -c '{live_rounds, checkpoint_ms, final_rsync, stopped_until_done_ms}' "$out/send.json"; cat "$out/receive.json"
jq -c '.turns[0] | {state, ms: ((.end_us - .start_us) / 1000 | floor)}' "$out/turn.summary.json"; echo "resend lines: $(wc -l < "$out/resend.txt")"
T "./gate.sh rm $name; rm -rf /run/gate/ckpt/in-${tside:+$tside-}$name"
if [ "${#fail[@]}" -gt 0 ]; then echo "FAIL: $(tr '\n' ';' < "$out/failures.txt")"; exit 1; fi
echo PASS

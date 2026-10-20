#!/usr/bin/env bash
# After a migration: shows that the background processes and the heartbeat connection are still the same ones.
# Usage: check-workload.sh IP TOKEN
ip=$1 token=$2
curl -s -m 30 -X POST -H "X-Jarvis-Exec-Token: $token" -H 'content-type: application/json' -d '{"command":"ps -eo pid,etimes,rss,args | grep -v \"ps -eo\"; echo; tail -n 3 /work/memcheck.log; grep -c BAD /work/memcheck.log | sed \"s/^/memcheck BAD lines: /\"; echo; tail -n 5 /work/heart.log; echo; tail -n 2 /work/counter.log; awk '"'"'NR>1{d=$1-p; if (d>1.5) print \"counter gap \" d \" s at \" $1} {p=$1}'"'"' /work/counter.log"}' "http://$ip:7070/exec" | jq -r '.stdout, .stderr'

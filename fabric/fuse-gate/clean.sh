#!/usr/bin/env bash
# Removes every gate sandbox, relay and received image on this host (after an interrupted run)
for p in $(ps -eo pid,args | awk '/sandboxd migrate-relay|gate\.sh (receive|send)|migrate\.sh|mapcount-mig\.sh|stale-mig\.sh/ && !/awk/ {print $1}'); do
  [ "$p" = "$$" ] || kill "$p" 2>/dev/null
done
sleep 1
for c in $(runsc --root /run/gate/runsc list -quiet 2>/dev/null); do
  runsc --root /run/gate/runsc kill "$c" KILL 2>/dev/null; runsc --root /run/gate/runsc delete -force "$c" 2>/dev/null
done
rm -rf /run/gate/ckpt/in-*
echo "sandboxes left: $(runsc --root /run/gate/runsc list -quiet 2>/dev/null | wc -l)"

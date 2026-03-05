#!/bin/bash
# Rapid output script for testing render throttle behavior
# Outputs 200 lines as fast as possible, then pauses, then outputs more

echo "=== Phase 1: Rapid burst (200 lines) ==="
for i in $(seq 1 200); do
  echo "line $i: $(date +%H:%M:%S.%N)"
done

echo ""
echo "=== Phase 2: Pause 2s ==="
sleep 2

echo "=== Phase 3: Rapid burst with progress overwrite ==="
for i in $(seq 1 100); do
  printf "\rProcessing item %3d/100..." "$i"
done
echo ""
echo "=== Done ==="

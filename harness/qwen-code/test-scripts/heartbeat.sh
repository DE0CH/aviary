#!/bin/bash
# Heartbeat script for testing interactive shell PTY behavior
# - Emits a numbered line every second
# - Writes to both stdout and a log file for verification
# - Handles SIGTERM/SIGINT gracefully

LOG="/tmp/heartbeat_test.log"
echo "started at $(date)" > "$LOG"

cleanup() {
  echo "received signal, shutting down" >> "$LOG"
  echo "[heartbeat] shutting down"
  exit 0
}

trap cleanup SIGTERM SIGINT

i=1
while true; do
  echo "[heartbeat] tick $i ($(date +%H:%M:%S))"
  echo "tick $i at $(date +%H:%M:%S)" >> "$LOG"
  i=$((i + 1))
  sleep 1
done

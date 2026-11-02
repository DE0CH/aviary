#!/usr/bin/env bash
# WP-57: does rsync's quick check (size + mtime, as sandboxd's sync_scratch and gate.sh send-scratch use it) see a
# write made through a MAP_SHARED mapping of the file? Run on a host, as root, in a directory on the scratch disk's
# filesystem (default /var/lib/gate/mmap-rsync). Prints one JSON line.
# Steps: map a 1 MiB file MAP_SHARED, write slot 0's length field (the page is now dirty), rsync to dst (a full copy),
# then write slot 0 again through the same mapping, as the sentry's read(2) of /dev/fuse writes a request into the
# journal's mapped slot; rsync again (the final delta) and compare the two files. With WRITEBACK=1 the page is written
# back (fsync) between the first rsync and the second write, so the second write faults and updates mtime.
set -euo pipefail
d=${1:-/var/lib/gate/mmap-rsync}; rm -rf "$d"; mkdir -p "$d/src" "$d/dst"
python3 - "$d/src/journal" "${WRITEBACK:-0}" "$d" <<'EOF'
import json, mmap, os, struct, subprocess, sys, time
path, writeback, d = sys.argv[1], sys.argv[2] == "1", sys.argv[3]
fd = os.open(path, os.O_RDWR | os.O_CREAT, 0o600)
os.ftruncate(fd, 1 << 20)
m = mmap.mmap(fd, 1 << 20, mmap.MAP_SHARED, mmap.PROT_READ | mmap.PROT_WRITE)
m[0:4] = struct.pack("<I", 0)          # the worker zeroes `len` before its read
def rs():
    out = subprocess.run(["rsync", "-aHAX", "--numeric-ids", "--delete", "--stats", d + "/src/", d + "/dst/"],
                         check=True, capture_output=True, text=True).stdout
    return int(next(l for l in out.splitlines() if l.startswith("Number of regular files transferred")).split(":")[1].replace(",", ""))
mt0 = os.stat(path).st_mtime_ns
first = rs()
time.sleep(1.1)                        # the next write is in a later second and a later timestamp tick
if writeback:
    os.fsync(fd)
m[0:56] = struct.pack("<IIQ", 56, 36, 741) + bytes(40)   # a 56-byte FUSE_INTERRUPT request lands in the slot
mt1 = os.stat(path).st_mtime_ns
final = rs()
with open(d + "/dst/journal", "rb") as f:
    dst_len = struct.unpack("<I", f.read(4))[0]
print(json.dumps({"writeback_between": writeback, "first_rsync_files": first, "mtime_changed_by_second_write": mt1 != mt0,
                  "final_rsync_files": final, "src_len": struct.unpack("<I", m[0:4])[0], "dst_len": dst_len,
                  "files_equal": open(path, "rb").read() == open(d + "/dst/journal", "rb").read()}))
EOF

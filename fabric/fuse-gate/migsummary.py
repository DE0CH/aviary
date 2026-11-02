#!/usr/bin/env python3
"""One row per migration run (migrate.sh outputs), plus per-configuration statistics.
  final_ms      the pre-copy's final round: the sandbox stopped, the last dirty pages sent (runsc restore's figure)
  stopped_ms    LAYOUT=scratch only: checkpoint end -> SBXDONE (the final rsync delta while stopped)
  pause_ms      what the sandbox's processes see as stopped: final_ms + stopped_ms + restore_ms
  bytes         the pre-copy stream; final_bytes = bytes moved while stopped (scratch: the final rsync delta; the
                stream's last-round share is not reported by runsc, so for tmpfs only the stream total is given)
  stream_final_page_bytes, stream_final_zero_bytes  the pre-copy stream's pages and zero ranges sent while stopped
                (reported by runsc from precopy-20261102.1 on; null before)
  scratch_differing  LAYOUT=scratch: scratch files that differed from the source after the final delta (WP-57)
usage: migsummary.py RESULTS_DIR > summary.json   (RESULTS_DIR holds mig-*/)"""
import glob, json, os, statistics, sys

def load(p):
    try:
        return json.load(open(p))
    except Exception:
        return None

rows = []
for d in sorted(glob.glob(os.path.join(sys.argv[1], "mig-*"))):
    t, rc = load(f"{d}/times.json") or {}, load(f"{d}/receive.json") or {}
    sd, dr, pr = load(f"{d}/send.json"), load(f"{d}/drive.json"), load(f"{d}/probe.json")
    fails = [l for l in open(f"{d}/failures.txt").read().splitlines() if l.strip()] if os.path.exists(f"{d}/failures.txt") else []
    fill = open(f"{d}/fill.txt").read().split("\n")[0] if os.path.exists(f"{d}/fill.txt") else ""
    stopped = sd["stopped_until_done_ms"] if sd else 0
    r = {"run": os.path.basename(d)[4:], "layout": t.get("layout"), "runsc_extra": t.get("runsc_extra", ""), "hold": t.get("hold", 1), "mode": t.get("mode", "fuse"),
         "fill": fill, "rounds": rc.get("rounds"), "bytes": rc.get("bytes"), "final_ms": rc.get("final_ms"),
         "restore_ms": rc.get("restore_ms"), "stopped_ms": stopped,
         "pause_ms": round(rc.get("final_ms", 0) + stopped + rc.get("restore_ms", 0), 1) if rc else None,
         "live_rsync_bytes": sum(x["bytes"] for x in sd["live_rounds"]) if sd else None,
         "final_bytes": sd["final_rsync"]["bytes"] if sd else None,
         "stream_final_page_bytes": rc.get("final_page_bytes"), "stream_final_zero_bytes": rc.get("final_zero_bytes"),
         "scratch_differing": sd.get("files_differing_after_final") if sd else None,
         "stat_wait_s": pr.get("stat_s") if pr else None,
         "turns": [x["result"] for x in dr["turns"]] if dr else None, "claude_exit": dr.get("exit") if dr else None,
         "failures": fails}
    rows.append(r)

def stats(v):
    v = [x for x in v if x is not None]
    if not v:
        return None
    return {"n": len(v), "min": min(v), "median": round(statistics.median(v), 1), "max": max(v)}

groups = {}
for r in rows:
    k = f"{r['layout']} hold={r['hold']} mode={r['mode']} {'full' if r['fill'] else 'empty'}" + (f" {r['runsc_extra']}" if r["runsc_extra"] else "")
    groups.setdefault(k, []).append(r)
out = {"groups": {k: {"runs": len(v), "failed": [x["run"] for x in v if x["failures"]],
                      "final_ms": stats([x["final_ms"] for x in v]), "pause_ms": stats([x["pause_ms"] for x in v]),
                      "restore_ms": stats([x["restore_ms"] for x in v]), "bytes": stats([x["bytes"] for x in v]),
                      "final_bytes": stats([x["final_bytes"] for x in v]),
                      "stream_final_page_bytes": stats([x["stream_final_page_bytes"] for x in v]),
                      "stream_final_zero_bytes": stats([x["stream_final_zero_bytes"] for x in v]),
                      "live_rsync_bytes": stats([x["live_rsync_bytes"] for x in v]),
                      "stat_wait_s": stats([x["stat_wait_s"] for x in v])} for k, v in groups.items()},
       "runs": rows}
print(json.dumps(out, indent=1))

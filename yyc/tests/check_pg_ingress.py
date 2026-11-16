#!/usr/bin/env python3
"""The old cluster's db/pg-ingress only grows while the old stack runs (yyc-ai/refactor spec-cutover 10.1 step 1,
DESIGN 15.5 B3): every (source, port) it admitted at BASE it still admits now. Its 10.244.0.0/16 rules (the old
stack's pods, the old fabric's JuiceFS mount pods) and 10.201.0.0/24 (the old fabric's hosts) stay until A6, whose
teardown commit carries `Pg-Ingress-Shrink: <why>` in its message to pass this check.

  check_pg_ingress.py BASE [HEAD]   (needs yq; reads news-stream/deploy/db/postgres.yaml at both commits)
"""
import json, subprocess, sys

FILE = "news-stream/deploy/db/postgres.yaml"


def pairs(rev):
    src = subprocess.run(["git", "show", f"{rev}:{FILE}"], capture_output=True, text=True)
    if src.returncode:
        return set()
    out = subprocess.run(["yq", "ea", "-o=json", "-I=0",
                          '[.] | map(select(.kind == "NetworkPolicy" and .metadata.name == "pg-ingress"))'],
                         input=src.stdout, capture_output=True, text=True, check=True)
    got = set()
    for np in json.loads(out.stdout):
        for rule in np["spec"].get("ingress", []):
            ports = [json.dumps(p, sort_keys=True) for p in rule.get("ports", [])] or ["all"]
            for peer in rule.get("from", []):
                for p in ports:
                    got.add((json.dumps(peer, sort_keys=True), p))
    return got


def main():
    base, head = sys.argv[1], (sys.argv[2] if len(sys.argv) > 2 else "HEAD")
    msgs = subprocess.run(["git", "log", "--format=%B", f"{base}..{head}"], capture_output=True, text=True).stdout
    lost = pairs(base) - pairs(head)
    if lost and "Pg-Ingress-Shrink:" not in msgs:
        for peer, port in sorted(lost):
            print(f"::error::db/pg-ingress no longer admits {peer} on {port} (the change must only add; "
                  "a deliberate removal carries a Pg-Ingress-Shrink: trailer)")
        sys.exit(1)
    print("pg-ingress: nothing removed" if not lost else f"pg-ingress: {len(lost)} rule(s) removed under Pg-Ingress-Shrink")


if __name__ == "__main__":
    main()

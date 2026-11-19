#!/usr/bin/env python3
"""Every image yyc-ai/deploy pins comes from a commit whose own CI passed (the YYC AI skill: TDD, the suites gate
what ships). Refuses a pin when that commit has no finished successful run of its workflow:

  aviary sha12 (pigeon, magpie, hermit, sandboxd)  DE0CH/aviary        ci.yml          (lint, every test, e2e)
  herald sha12 (its own aviary commit)             DE0CH/aviary        ci.yml
  pigeon-app sha12                                 DE0CH/claudecodeui  pigeon-app.yml  (the paseo suite first)
  jarvis3-chat sha12                               DE0CH/jarvis3       chat.yml        (tests, conformance)
  yyc-drive sha12                                  DE0CH/yyc-drive     yyc-drive.yml   (the server's and the page's tests)

A re-run that passed counts. Stdlib only; GH_TOKEN (the workflow's own token) lifts the API's anonymous rate limit.

  check_pins_green.py [--folder yyc-ai/deploy]"""
import argparse, json, os, re, sys, urllib.request

AVIARY = ("pigeon", "magpie", "hermit", "sandboxd")
SOURCES = [("DE0CH/aviary", "ci.yml", AVIARY),
           ("DE0CH/aviary", "ci.yml", ("herald",)),
           ("DE0CH/claudecodeui", "pigeon-app.yml", ("pigeon-app",)),
           ("DE0CH/jarvis3", "chat.yml", ("jarvis3-chat",)),
           ("DE0CH/yyc-drive", "yyc-drive.yml", ("yyc-drive",))]


def pins(kustomization):
    tags = dict(re.findall(r"\{name: ([a-z0-9-]+), newName: [^,]+, newTag: \"?([0-9a-f]+)\"?\}", kustomization))
    out = []
    for repo, wf, names in SOURCES:
        shas = {tags.get(n) for n in names}
        if len(shas) != 1 or None in shas:
            sys.exit(f"::error::kustomization.yaml: {', '.join(names)} are not one {repo} commit: {tags}")
        out.append((repo, wf, shas.pop()))
    return out


def problems(pinned, get):
    out = []
    for repo, wf, short in pinned:
        full = get(f"/repos/{repo}/commits/{short}")["sha"]
        runs = get(f"/repos/{repo}/actions/workflows/{wf}/runs?head_sha={full}&per_page=50")["workflow_runs"]
        if any(r["status"] == "completed" and r["conclusion"] == "success" for r in runs):
            continue
        if not runs:
            out.append(f"{repo}@{short}: no {wf} run for this commit")
        elif any(r["status"] != "completed" for r in runs):
            out.append(f"{repo}@{short}: {wf} has not finished ({runs[0]['html_url']})")
        else:
            out.append(f"{repo}@{short}: {wf} concluded {runs[0]['conclusion']} ({runs[0]['html_url']})")
    return out


def github(path):
    req = urllib.request.Request("https://api.github.com" + path, headers={
        "Accept": "application/vnd.github+json", "User-Agent": "yyc-ai-check",
        **({"Authorization": f"Bearer {os.environ['GH_TOKEN']}"} if os.environ.get("GH_TOKEN") else {})})
    with urllib.request.urlopen(req, timeout=30) as r:
        return json.load(r)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--folder", default="yyc-ai/deploy")
    a = ap.parse_args()
    pinned = pins(open(os.path.join(a.folder, "kustomization.yaml")).read())
    bad = problems(pinned, github)
    for p in bad:
        print(f"::error::pin {p}")
    if not bad:
        print("pins green: " + ", ".join(f"{r}@{s} ({w})" for r, w, s in pinned))
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()

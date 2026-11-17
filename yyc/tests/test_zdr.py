#!/usr/bin/env python3
"""YYC AI's zero-data-retention guard (Deyao, 2026-10-20: staff get a fixed shortlist, none of it non-ZDR, and every
request forces ZDR itself; yyc-ai/refactor spec-cutover 10.1.1, spec-platform 13.6). It checks
yyc-ai/deploy/egress/egress.json (staged: yyc-ai/next/): `models[].id`, `defaultModel`, `smallModel`, `titleModel`,
`embedding.id` (the conversation search's model), and
each slug in `models[].providers` (the endpoints egress pins the model to) being a ZDR endpoint of that model; the
route `model` goes to openrouter.ai with `model: true` and no other route names openrouter.ai; the egress source is the
`jarvis3-chat` tag of that folder's kustomization.yaml (a sha12, resolved to the full commit through the GitHub API; a
404 or an ambiguous prefix fails).

Every model staff can pick (`models[].id`) must also take images (OpenRouter's public GET /api/v1/models,
`architecture.input_modalities`): the agent reads images (staff uploads, pictures it makes), and a text-only model
then fails every call of the conversation with "No endpoints found that support image input" (GLM 5.3, 2026-11-13).

Fails when a listed model has no Zero Data Retention endpoint on OpenRouter (public GET /api/v1/endpoints/zdr), when
OpenRouter is reachable other than through a forced-ZDR model route, or when a pinned jarvis3 commit lacks the forcing
literals (chat/src/egress/modelbody.ts FORCED_PROVIDER, proxy.ts `isModel` and `ModelBodyRewriter`, and the egress test).

  python3 yyc-ai/tests/test_zdr.py   (stdlib only; needs internet; GITHUB_TOKEN or GH_TOKEN for the commit lookup)
"""
import json, os, re, sys, urllib.request

HERE = os.path.dirname(os.path.abspath(__file__))
YYC_AI = os.path.dirname(HERE)
UA = {"User-Agent": "yyc-ai-zdr-test/2"}


def http_get(url):
    h = dict(UA)
    tok = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if tok and url.startswith("https://api.github.com/"):
        h["Authorization"] = "Bearer " + tok
    with urllib.request.urlopen(urllib.request.Request(url, headers=h), timeout=60) as r:
        return r.read().decode()


def stack(root=YYC_AI):
    """the stack's folder, or None"""
    return next((os.path.join(root, d) for d in ("deploy", "next") if os.path.exists(os.path.join(root, d, "egress", "egress.json"))), None)


def jarvis3_literals(sha, get, check):
    raw = f"https://raw.githubusercontent.com/DE0CH/jarvis3/{sha}/chat"
    short = sha[:12]
    try:
        body, proxy, test = get(raw + "/src/egress/modelbody.ts"), get(raw + "/src/egress/proxy.ts"), get(raw + "/test/egress.test.ts")
    except Exception as e:  # noqa: BLE001 - a missing file is the failure
        check(False, f"jarvis3 {short}: the egress sources ({e})")
        return
    check('export const FORCED_PROVIDER = { zdr: true, data_collection: "deny" }' in body,
          f"jarvis3 {short}: FORCED_PROVIDER = zdr true, data_collection deny")
    # the body of a model call has one way upstream: through the streaming rewriter, whose end() writes the forced options
    check("const isModel = route.model === true;" in proxy
          and 'const rewrite = isModel && req.method !== "GET" && req.method !== "HEAD" ? new ModelBodyRewriter(' in proxy
          and "rewrite.push(chunk)" in proxy and "rewrite.end()" in proxy and "if (!rewrite) {" in proxy
          and re.search(r"\.\.\.FORCED_PROVIDER,\n\s*};", body) is not None,
          f"jarvis3 {short}: every route flagged model rewrites each request body with ModelBodyRewriter")
    check("zero data retention is forced into every request body" in test, f"jarvis3 {short}: its egress test covers the forcing")


def routes_ok(where, routes, check):
    for r in routes:
        if "openrouter.ai" in r["upstream"]:
            check(r.get("model") is True, f"{where}: route {r['name']} -> {r['upstream']} is a forced-ZDR model route")


def run(root=YYC_AI, get=http_get):
    fails = []

    def check(ok, msg):
        print(("ok   " if ok else "FAIL ") + msg)
        if not ok:
            fails.append(msg)

    zdr, zdr_tags = {}, {}
    for e in json.loads(get("https://openrouter.ai/api/v1/endpoints/zdr"))["data"]:
        zdr.setdefault(e["model_id"], []).append(e["provider_name"])
        zdr_tags.setdefault(e["model_id"], set()).add(e.get("tag"))

    def models_ok(where, ids):
        for m in sorted(ids):
            check(bool(zdr.get(m)), f"{where}: {m} has ZDR endpoints {sorted(set(zdr.get(m, [])))}")

    new = stack(root)
    check(new is not None, "the new stack's egress.json exists")
    if new:
        e = json.load(open(os.path.join(new, "egress", "egress.json")))
        # the conversation search's embedding model (`embedding.id`) sees every message too: ZDR like the others
        models_ok("new stack", {m["id"] for m in e["models"]} | {e[k] for k in ("defaultModel", "smallModel", "titleModel") if e.get(k)}
                  | ({e["embedding"]["id"]} if e.get("embedding") else set()))
        takes = {m["id"]: (m.get("architecture") or {}).get("input_modalities") or [] for m in json.loads(get("https://openrouter.ai/api/v1/models"))["data"]}
        for m in e["models"]:
            check("image" in takes.get(m["id"], []), f"new stack: {m['id']} takes images (input: {takes.get(m['id'])})")
        for m in e["models"]:
            for tag in m.get("providers", []):
                check(tag in zdr_tags.get(m["id"], ()), f"new stack: {m['id']} pinned endpoint {tag} is a ZDR endpoint")
        routes = e["routes"]
        model = [r for r in routes if r["name"] == "model"]
        check(len(model) == 1 and "openrouter.ai" in model[0]["upstream"] and model[0].get("model") is True,
              "new stack: route model goes to openrouter.ai with model: true")
        routes_ok("new stack", routes, check)
        kust = open(os.path.join(new, "kustomization.yaml")).read()
        tag = re.search(r"\{name: jarvis3-chat, newName: [^,]+, newTag: \"?([0-9a-f]{7,40})\"?\}", kust)
        check(bool(tag), "new stack: kustomization.yaml pins jarvis3-chat by a commit")
        if tag:
            try:
                sha = json.loads(get(f"https://api.github.com/repos/DE0CH/jarvis3/commits/{tag.group(1)}"))["sha"]
            except Exception as ex:  # noqa: BLE001
                sha = None
                check(False, f"new stack: jarvis3 commit {tag.group(1)} resolves ({ex})")
            if sha:
                check(sha.startswith(tag.group(1)), f"new stack: jarvis3 {tag.group(1)} is {sha}")
                jarvis3_literals(sha, get, check)
    print(f"\n{len(fails)} failure(s)")
    return fails


if __name__ == "__main__":
    sys.exit(1 if run() else 0)

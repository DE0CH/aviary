#!/usr/bin/env python3
"""YYC AI's zero-data-retention guard (Deyao, 2026-10-20: staff get a fixed shortlist, none of it non-ZDR, and every
request forces ZDR itself; yyc/refactor spec-cutover 10.1.1, spec-platform 13.6). It checks
yyc/deploy/egress/egress.json (staged: yyc/next/): `models[].id`, `defaultModel`, `smallModel`, `titleModel`,
`embedding.id` (the conversation search's model), `transcription` (speech to text) and its pinned endpoints, and
each slug in `models[].providers` (the endpoints egress pins the model to) being a ZDR endpoint of that model; the
route `model` goes to openrouter.ai with `model: true` and no other route names openrouter.ai; the egress source is
this repository's egress/ (deployed with the folder, docs/DEPLOY.md).

Every model staff can pick (`models[].id`) must also take images (OpenRouter's public GET /api/v1/models,
`architecture.input_modalities`): the agent reads images (staff uploads, pictures it makes), and a text-only model
then fails every call of the conversation with "No endpoints found that support image input" (GLM 5.3, 2026-11-13).

Fails when a listed model has no Zero Data Retention endpoint on OpenRouter (public GET /api/v1/endpoints/zdr), when
OpenRouter is reachable other than through a forced-ZDR model route, or when egress lacks the forcing literals
(egress/src/egress/modelbody.ts FORCED_PROVIDER, proxy.ts `isModel` and `ModelBodyRewriter`, and the egress test).

  python3 yyc/tests/test_zdr.py   (stdlib only; needs internet)
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


# the egress deployed with the folder: this repository's egress/ (docs/DEPLOY.md)
EGRESS = os.path.join(os.path.dirname(YYC_AI), "egress")


def egress_literals(src, check):
    """egress's sources at `src` force ZDR on every model call"""
    try:
        body = open(os.path.join(src, "src/egress/modelbody.ts")).read()
        proxy = open(os.path.join(src, "src/egress/proxy.ts")).read()
        test = open(os.path.join(src, "test/egress.test.ts")).read()
    except OSError as e:
        check(False, f"egress: the sources ({e})")
        return
    check('export const FORCED_PROVIDER = { zdr: true, data_collection: "deny" }' in body,
          "egress: FORCED_PROVIDER = zdr true, data_collection deny")
    # the body of a model call has one way upstream: through the streaming rewriter, whose end() writes the forced options
    check("const isModel = route.model === true;" in proxy
          and 'const rewrite = isModel && req.method !== "GET" && req.method !== "HEAD" ? new ModelBodyRewriter(' in proxy
          and "rewrite.push(chunk)" in proxy and "rewrite.end()" in proxy and "if (!rewrite) {" in proxy
          and re.search(r"\.\.\.FORCED_PROVIDER,\n\s*};", body) is not None,
          "egress: every route flagged model rewrites each request body with ModelBodyRewriter")
    check("zero data retention is forced into every request body" in test, "egress: its egress test covers the forcing")


def routes_ok(where, routes, check):
    for r in routes:
        if "openrouter.ai" in r["upstream"]:
            check(r.get("model") is True, f"{where}: route {r['name']} -> {r['upstream']} is a forced-ZDR model route")


def run(root=YYC_AI, get=http_get, egress=EGRESS):
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
                  | ({e["embedding"]["id"]} if e.get("embedding") else set())
                  | ({e["transcription"]["id"]} if e.get("transcription") else set()))
        # the speech-to-text model hears whatever a sandbox sends it: pinned to ZDR endpoints of that model
        if e.get("transcription"):
            for tag in e["transcription"].get("providers", []):
                check(tag in zdr_tags.get(e["transcription"]["id"], ()), f"new stack: {e['transcription']['id']} pinned endpoint {tag} is a ZDR endpoint")
        listed = json.loads(get("https://openrouter.ai/api/v1/models"))["data"]
        takes = {m["id"]: (m.get("architecture") or {}).get("input_modalities") or [] for m in listed}
        efforts = {m["id"]: (m.get("reasoning") or {}).get("supported_efforts") or [] for m in listed}
        for m in e["models"]:
            check("image" in takes.get(m["id"], []), f"new stack: {m['id']} takes images (input: {takes.get(m['id'])})")
        # the menu's thinking levels of a model are the ones OpenRouter's record of that model takes, never another
        # model's list (lessons/139: a level the endpoint lacks changes nothing)
        for m in e["models"]:
            if m.get("thinking"):
                lacks = [t for t in m["thinking"] if t not in efforts.get(m["id"], [])]
                check(not lacks, f"new stack: {m['id']} takes every thinking level it lists {m['thinking']}"
                                 f" (OpenRouter: {efforts.get(m['id'])}; lacks {lacks})")
        for m in e["models"]:
            for tag in m.get("providers", []):
                check(tag in zdr_tags.get(m["id"], ()), f"new stack: {m['id']} pinned endpoint {tag} is a ZDR endpoint")
        routes = e["routes"]
        model = [r for r in routes if r["name"] == "model"]
        check(len(model) == 1 and "openrouter.ai" in model[0]["upstream"] and model[0].get("model") is True,
              "new stack: route model goes to openrouter.ai with model: true")
        routes_ok("new stack", routes, check)
        egress_literals(egress, check)
    print(f"\n{len(fails)} failure(s)")
    return fails


if __name__ == "__main__":
    sys.exit(1 if run() else 0)

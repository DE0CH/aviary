#!/usr/bin/env python3
"""OpenRouter side of YYC AI (idempotent; moved from staff-ai/ 2026-10-20): the API key `yyc-chat` (used ONLY by this app, so OpenRouter's activity
shows YYC staff AI's spend on its own; EXPENSES.md) and the guardrail `yyc-chat` assigned to that key:

  - Zero Data Retention enforced for every model group (Anthropic, OpenAI, Google, xAI, other), and no endpoint that
    keeps prompts "for safety monitoring" (allow_safety_retention_* false)
  - no endpoint that trains on or publishes prompts (enable_*_training / publication false)
  - only the models staff can use: egress.json's models, defaultModel, smallModel and titleModel, and the conversation
    search's embedding model (../deploy/egress/, staged ../next/egress/; tests/test_openrouter_guardrail.py)

jarvis3's egress also forces provider {zdr: true, data_collection: "deny"} into every request (../README.md "Zero
data retention"); the guardrail holds even if that were missing. A key made here is written to OUT/openrouter-key (mode 600) for
store-put.py as YYC_AI_OPENROUTER_KEY; nothing secret is printed.

  YYC_OPENROUTER_MANAGEMENT_KEY=… infra/openrouter.py OUT
  … infra/openrouter.py OUT --rotate          make a second key `yyc-chat-next` under the same guardrail (deploy it)
  … infra/openrouter.py OUT --finish-rotate   delete the old `yyc-chat` and rename `yyc-chat-next` to `yyc-chat`
  YYC_AI_OPENROUTER_KEY=… infra/openrouter.py OUT --verify
        live checks on the key itself: a ZDR request on a listed model passes; a first-party (non-ZDR) endpoint, the
        same with data_collection allow / zdr false, and a model off the list are refused. Exit 1 on any failure.
"""
import json, os, sys, urllib.request, urllib.error

API = "https://openrouter.ai/api/v1"
NAME = "yyc-chat"
LIMIT_USD = 100  # a safety stop per calendar month (CLAUDE.md "Budgets are safety stops"), not a plan
HERE = os.path.dirname(os.path.abspath(__file__))


def _models():
    ids = set()
    for d in ("deploy", "next"):
        p = os.path.join(HERE, "..", d, "egress", "egress.json")
        if os.path.exists(p):
            e = json.load(open(p))
            ids |= {m["id"] for m in e["models"]} | {e[k] for k in ("defaultModel", "smallModel", "titleModel") if e.get(k)}
            # the conversation search's embedding model: the sandboxes' and pigeon's /v1/embeddings on this key
            if e.get("embedding"):
                ids.add(e["embedding"]["id"])
            # the speech-to-text model: the sandboxes' /v1/audio/transcriptions on this key
            if e.get("transcription"):
                ids.add(e["transcription"]["id"])
            break
    if not ids:
        raise SystemExit("no model list: egress.json not found")
    return sorted(ids)


MODELS = _models()


def call(method, path, body=None):
    req = urllib.request.Request(API + path, method=method, data=json.dumps(body).encode() if body is not None else None,
                                 headers={"Authorization": "Bearer " + os.environ["YYC_OPENROUTER_MANAGEMENT_KEY"],
                                          "Content-Type": "application/json", "User-Agent": "yyc-ai-infra/1"})
    try:
        return json.load(urllib.request.urlopen(req))
    except urllib.error.HTTPError as e:
        raise SystemExit(f"{method} {path}: HTTP {e.code} {e.read().decode()[:400]}")


def verify():
    """the key's guardrail, checked with real requests (a few tokens each on yyc-chat)"""
    msg = [{"role": "user", "content": "Say OK."}]
    model = next((m for m in MODELS if "haiku" in m), MODELS[0])
    zdr = {"zdr": True, "data_collection": "deny"}
    first_party = {"only": ["anthropic"], "allow_fallbacks": False}
    cases = [  # (what, body, must pass)
        ("ZDR request on a listed model", {"model": model, "messages": msg, "max_tokens": 16, "provider": zdr}, True),
        ("first-party Anthropic endpoint (not ZDR)", {"model": model, "messages": msg, "max_tokens": 16,
                                                     "provider": first_party}, False),
        ("first-party endpoint asking for zdr false, data_collection allow",
         {"model": model, "messages": msg, "max_tokens": 16,
          "provider": {**first_party, "zdr": False, "data_collection": "allow"}}, False),
        ("a model off the list", {"model": "openai/gpt-4o-mini", "messages": msg, "max_tokens": 16}, False),
    ]
    bad = 0
    for what, body, want in cases:
        req = urllib.request.Request(API + "/chat/completions", method="POST", data=json.dumps(body).encode(),
                                     headers={"Authorization": "Bearer " + os.environ["YYC_AI_OPENROUTER_KEY"],
                                              "Content-Type": "application/json", "User-Agent": "yyc-ai-infra/1"})
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                code, detail = r.status, "provider " + str(json.load(r).get("provider"))
        except urllib.error.HTTPError as e:
            code, detail = e.code, e.read().decode()[:200]
        ok = (code == 200) == want
        bad += not ok
        print(f"{'ok  ' if ok else 'FAIL'} {what}: HTTP {code} ({'must pass' if want else 'must be refused'}) {detail}")
    sys.exit(1 if bad else 0)


def main():
    out = sys.argv[1]
    mode = sys.argv[2] if len(sys.argv) > 2 else ""
    if mode == "--verify":
        verify()
    os.makedirs(out, mode=0o700, exist_ok=True)
    if mode == "--finish-rotate":
        keys = call("GET", "/keys")["data"]
        nxt = next(k for k in keys if k.get("name") == NAME + "-next")
        for k in keys:
            if k.get("name") == NAME:
                call("DELETE", f"/keys/{k['hash']}"); print(f"deleted old key {NAME} ({k['hash'][:12]}…)")
        call("PATCH", f"/keys/{nxt['hash']}", {"name": NAME}); print(f"renamed {NAME}-next ({nxt['hash'][:12]}…) to {NAME}")
        sys.exit(0)
    if mode == "--rotate":
        NAME_KEY = NAME + "-next"
    else:
        NAME_KEY = NAME

    keys = call("GET", "/keys")["data"]
    key = next((k for k in keys if k.get("name") == NAME_KEY), None)
    if key is None:
        r = call("POST", "/keys", {"name": NAME_KEY, "limit": LIMIT_USD, "limit_reset": "monthly"})
        path = os.path.join(out, "openrouter-key")
        with open(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as f:
            f.write(r["key"])
        key = r["data"]
        print(f"made key {NAME_KEY} (hash {key['hash'][:12]}…), written to {path}: store it as YYC_AI_OPENROUTER_KEY")
    else:
        call("PATCH", f"/keys/{key['hash']}", {"limit": LIMIT_USD, "limit_reset": "monthly"})
        print(f"key {NAME_KEY} exists (hash {key['hash'][:12]}…)")

    policy = {
        "name": NAME,
        "description": "YYC staff AI (ai.yyc-internal.com): ZDR endpoints only, no training/publication, only the listed models",
        "enforce_zdr_anthropic": True, "enforce_zdr_openai": True, "enforce_zdr_google": True,
        "enforce_zdr_xai": True, "enforce_zdr_other": True,
        "allow_safety_retention_google": False, "allow_safety_retention_other": False,
        "enable_paid_model_training": False, "enable_free_model_training": False, "enable_free_model_publication": False,
        "allowed_models": MODELS,
    }
    grs = call("GET", "/guardrails")["data"]
    gr = next((g for g in grs if g.get("name") == NAME), None)
    if gr is None:
        gr = call("POST", "/guardrails", policy)["data"]
        print(f"made guardrail {NAME} ({gr['id']})")
    else:
        gr = call("PATCH", f"/guardrails/{gr['id']}", policy)["data"]
        print(f"updated guardrail {NAME} ({gr['id']})")

    assigned = call("GET", f"/guardrails/{gr['id']}/assignments/keys")["data"]
    if not any(a.get("key_hash") == key["hash"] for a in assigned):
        call("POST", f"/guardrails/{gr['id']}/assignments/keys", {"key_hashes": [key["hash"]]})
        print(f"assigned guardrail {NAME} to key {NAME_KEY}")
    print(json.dumps({k: gr.get(k) for k in policy if k not in ("name", "description")}))


if __name__ == "__main__":
    main()

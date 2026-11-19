#!/usr/bin/env python3
"""The ZDR guard (test_zdr.py) catches its seeded mutations (a pinned non-ZDR endpoint among them) (spec-cutover 10.0's checklist), offline: a fake
network answers OpenRouter's ZDR list and GitHub; the stacks are copies of the tree's in a temporary folder."""
import json, os, shutil, sys, tempfile, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import test_zdr  # noqa: E402

YYC_AI = os.path.dirname(HERE)
GOOD = {
    "/src/egress/modelbody.ts": 'export const FORCED_PROVIDER = { zdr: true, data_collection: "deny" };\n'
                                'const provider = {\n  ...p,\n  ...FORCED_PROVIDER,\n};',
    "/src/egress/proxy.ts": 'const isModel = route.model === true;\n'
                            'const rewrite = isModel && req.method !== "GET" && req.method !== "HEAD" ? new ModelBodyRewriter(plan) : null;\n'
                            "if (!rewrite) {\n  req.pipe(out);\n}\nparts = rewrite.push(chunk);\ntail = rewrite.end();",
    "/test/egress.test.ts": 'it("zero data retention is forced into every request body")',
}


def tree():
    root = tempfile.mkdtemp()
    shutil.copytree(test_zdr.stack(YYC_AI), os.path.join(root, "next"))
    return root


EFFORTS = ["max", "high", "low", "none"]


def fake(zdr_models, bad_sha=None, tags=None, text_only=(), efforts=None):
    """tags: {model: [endpoint slugs]} listed as ZDR endpoints (the pinned providers); efforts: {model: [levels]}
    OpenRouter's `reasoning.supported_efforts` (EFFORTS for a model not named)"""
    def get(url):
        if url.endswith("/endpoints/zdr"):
            return json.dumps({"data": [{"model_id": m, "provider_name": "P"} for m in zdr_models]
                               + [{"model_id": m, "provider_name": "P", "tag": t} for m, ts in (tags or {}).items() for t in ts]})
        if url.endswith("/api/v1/models"):
            return json.dumps({"data": [{"id": m, "architecture": {"input_modalities": ["text"] if m in text_only else ["text", "image"]},
                                         "reasoning": {"supported_efforts": (efforts or {}).get(m, EFFORTS)}}
                                        for m in set(zdr_models) | set(text_only)]})
        if url.startswith("https://api.github.com/repos/DE0CH/jarvis3/commits/"):
            return json.dumps({"sha": url.rsplit("/", 1)[1] + "0" * 28})
        for suffix, body in GOOD.items():
            if url.endswith(suffix):
                return body.replace("FORCED_PROVIDER", "PROVIDER") if bad_sha and bad_sha in url else body
        raise RuntimeError(f"unexpected {url}")
    return get


def models(root):
    e = json.load(open(os.path.join(root, "next", "egress", "egress.json")))
    return {m["id"] for m in e["models"]} | {e["defaultModel"], e["smallModel"], e["titleModel"]} \
        | ({e["embedding"]["id"]} if e.get("embedding") else set())


def pins(root):
    e = json.load(open(os.path.join(root, "next", "egress", "egress.json")))
    return {m["id"]: m.get("providers", []) for m in e["models"]}


def edit(root, f):
    p = os.path.join(root, "next", "egress", "egress.json")
    e = json.load(open(p))
    f(e)
    json.dump(e, open(p, "w"))


class Seeded(unittest.TestCase):
    def test_clean(self):
        root = tree()
        self.assertEqual(test_zdr.run(root, fake(models(root), tags=pins(root))), [])

    def test_non_zdr_small_model(self):
        root = tree()
        edit(root, lambda e: e.update(smallModel="vendor/no-zdr"))
        fails = test_zdr.run(root, fake(models(root) - {"vendor/no-zdr"}))
        self.assertTrue(any("vendor/no-zdr" in f for f in fails), fails)

    def test_non_zdr_embedding_model(self):
        # the conversation search's model sees every message: ZDR like the others (it takes no images)
        root = tree()
        edit(root, lambda e: e.update(embedding={"id": "vendor/embed-no-zdr"}))
        fails = test_zdr.run(root, fake(models(root) - {"vendor/embed-no-zdr"}, tags=pins(root)))
        self.assertTrue(any("vendor/embed-no-zdr" in f for f in fails), fails)
        self.assertFalse(any("embed-no-zdr takes images" in f for f in fails), fails)

    def test_pinned_endpoint_not_zdr(self):
        root = tree()
        edit(root, lambda e: e["models"][0].update(providers=["vendor/no-zdr"]))
        fails = test_zdr.run(root, fake(models(root), tags={}))
        self.assertTrue(any("vendor/no-zdr" in f for f in fails), fails)

    def test_text_only_model(self):
        root = tree()
        edit(root, lambda e: e["models"].append({"id": "vendor/text-only", "label": "T", "reasoning": "high", "thinking": ["high"]}))
        fails = test_zdr.run(root, fake(models(root), tags=pins(root), text_only={"vendor/text-only"}))
        self.assertTrue(any("vendor/text-only takes images" in f for f in fails), fails)

    def test_thinking_level_the_model_lacks(self):
        # lessons/139: a list copied from another model offers levels that change nothing
        root = tree()
        edit(root, lambda e: e["models"].append({"id": "vendor/three-levels", "label": "L", "reasoning": "high",
                                                 "thinking": ["none", "low", "high", "max"]}))
        fails = test_zdr.run(root, fake(models(root), tags=pins(root), efforts={"vendor/three-levels": ["low", "high", "max"]}))
        self.assertTrue(any("vendor/three-levels takes every thinking level" in f and "'none'" in f for f in fails), fails)

    def test_openrouter_route_without_model(self):
        root = tree()
        edit(root, lambda e: e["routes"][1].update(upstream="https://openrouter.ai/api"))
        fails = test_zdr.run(root, fake(models(root)))
        self.assertTrue(any("route tools" in f for f in fails), fails)

    def test_pin_without_forced_provider(self):
        root = tree()
        k = open(os.path.join(root, "next", "kustomization.yaml")).read()
        import re
        tag = re.search(r"name: jarvis3-chat, newName: [^,]+, newTag: \"?([0-9a-f]+)", k).group(1)
        fails = test_zdr.run(root, fake(models(root), bad_sha=tag))
        self.assertTrue(any("FORCED_PROVIDER" in f for f in fails), fails)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""The guardrail yyc-chat (yyc/infra/openrouter.py) lets through every model egress.json sends to OpenRouter on the
key yyc-chat: the chat models, the default, small and title models, and the conversation search's embedding model.
A model left off is refused by OpenRouter with a 404 ("Model blocked by guardrail"): the sandboxes' embeddings failed
so on 2026-11-17 and the search found no message by its meaning. Stdlib only, no network."""
import importlib.util, json, os, unittest

HERE = os.path.dirname(os.path.abspath(__file__))
EGRESS = os.path.join(HERE, "..", "deploy", "egress", "egress.json")


def load():
    spec = importlib.util.spec_from_file_location("openrouter", os.path.join(HERE, "..", "infra", "openrouter.py"))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)  # defines only: the run is under __main__
    return m


class Guardrail(unittest.TestCase):
    def test_every_model_egress_sends(self):
        e = json.load(open(EGRESS))
        sent = {m["id"] for m in e["models"]} | {e[k] for k in ("defaultModel", "smallModel", "titleModel") if e.get(k)}
        if e.get("embedding"):
            sent.add(e["embedding"]["id"])
        if e.get("transcription"):
            sent.add(e["transcription"]["id"])
        self.assertEqual(sorted(sent - set(load().MODELS)), [])

    def test_the_embedding_model_is_listed(self):
        e = json.load(open(EGRESS))
        self.assertIn("embedding", e, "egress.json names the search's embedding model")
        self.assertIn(e["embedding"]["id"], load().MODELS)

    def test_the_transcription_model_is_listed(self):
        # lessons/128: a new kind of model call on a key with a guardrail is refused until the guardrail lists it
        e = json.load(open(EGRESS))
        self.assertIn("transcription", e, "egress.json names the speech-to-text model")
        self.assertIn(e["transcription"]["id"], load().MODELS)


if __name__ == "__main__":
    unittest.main()

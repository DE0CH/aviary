#!/usr/bin/env python3
"""check_pins_green.py refuses a pin whose source commit's CI did not pass (stdlib only, no network: GitHub's API is a
fake). yyc-ai.yml check runs it before the live check."""
import os, sys, unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import check_pins_green as c  # noqa: E402

KUST = """images:
  - {name: pigeon, newName: ghcr.io/de0ch/pigeon, newTag: "aaaaaaaaaaaa"}
  - {name: magpie, newName: ghcr.io/de0ch/magpie, newTag: "aaaaaaaaaaaa"}
  - {name: hermit, newName: ghcr.io/de0ch/hermit, newTag: "aaaaaaaaaaaa"}
  - {name: sandboxd, newName: ghcr.io/de0ch/sandboxd, newTag: "aaaaaaaaaaaa"}
  - {name: herald, newName: ghcr.io/de0ch/herald, newTag: "eeeeeeeeeeee"}
  - {name: pigeon-app, newName: ghcr.io/de0ch/pigeon-app, newTag: "bbbbbbbbbbbb"}
  - {name: jarvis3-chat, newName: ghcr.io/de0ch/jarvis3-chat, newTag: "cccccccccccc"}
  - {name: yyc-drive, newName: ghcr.io/de0ch/yyc-drive, newTag: "ffffffffffff"}
"""


def fake(runs):
    """runs: {(repo, workflow): [(status, conclusion), ...]} for the pinned commit; every sha12 resolves to itself * 3"""
    def get(path):
        if "/commits/" in path:
            short = path.rsplit("/", 1)[1]
            return {"sha": short + short + short[:16]}
        repo = "/".join(path.split("/")[2:4])
        wf = path.split("/workflows/")[1].split("/")[0]
        assert "head_sha=" in path, path
        return {"workflow_runs": [{"status": s, "conclusion": k, "html_url": "u"} for s, k in runs.get((repo, wf), [])]}
    return get


GREEN = {("DE0CH/aviary", "ci.yml"): [("completed", "success")],
         ("DE0CH/claudecodeui", "pigeon-app.yml"): [("completed", "success")],
         ("DE0CH/jarvis3", "chat.yml"): [("completed", "success")],
         ("DE0CH/yyc-drive", "yyc-drive.yml"): [("completed", "success")]}


class Pins(unittest.TestCase):
    def test_pins_parsed(self):
        self.assertEqual(c.pins(KUST), [("DE0CH/aviary", "ci.yml", "aaaaaaaaaaaa"),
                                        ("DE0CH/aviary", "ci.yml", "eeeeeeeeeeee"),
                                        ("DE0CH/claudecodeui", "pigeon-app.yml", "bbbbbbbbbbbb"),
                                        ("DE0CH/jarvis3", "chat.yml", "cccccccccccc"),
                                        ("DE0CH/yyc-drive", "yyc-drive.yml", "ffffffffffff")])

    def test_aviary_tags_must_agree(self):
        with self.assertRaises(SystemExit):
            c.pins(KUST.replace('hermit, newTag: "aaaaaaaaaaaa"', 'hermit, newTag: "dddddddddddd"'))

    def test_herald_is_its_own_aviary_commit_and_required(self):
        # herald (aviary crates/herald) is pinned apart from the four services, and checked like them
        self.assertIn(("DE0CH/aviary", "ci.yml", "eeeeeeeeeeee"), c.pins(KUST))
        with self.assertRaises(SystemExit):
            c.pins(KUST.replace('  - {name: herald, newName: ghcr.io/de0ch/herald, newTag: "eeeeeeeeeeee"}\n', ""))

    def test_the_drive_is_pinned_and_its_own_ci_gates_it(self):
        # YYC Drive's image (DE0CH/yyc-drive): required, and refused when its workflow did not pass for that commit
        with self.assertRaises(SystemExit):
            c.pins(KUST.replace('  - {name: yyc-drive, newName: ghcr.io/de0ch/yyc-drive, newTag: "ffffffffffff"}\n', ""))
        runs = {**GREEN, ("DE0CH/yyc-drive", "yyc-drive.yml"): [("completed", "failure")]}
        p = c.problems(c.pins(KUST), fake(runs))
        self.assertEqual(len(p), 1)
        self.assertIn("DE0CH/yyc-drive@ffffffffffff", p[0])

    def test_all_green(self):
        self.assertEqual(c.problems(c.pins(KUST), fake(GREEN)), [])

    def test_failed_run(self):
        runs = {**GREEN, ("DE0CH/claudecodeui", "pigeon-app.yml"): [("completed", "failure")]}
        p = c.problems(c.pins(KUST), fake(runs))
        self.assertEqual(len(p), 1)
        self.assertIn("pigeon-app", p[0])
        self.assertIn("failure", p[0])

    def test_rerun_that_passed_counts(self):
        runs = {**GREEN, ("DE0CH/aviary", "ci.yml"): [("completed", "success"), ("completed", "failure")]}
        self.assertEqual(c.problems(c.pins(KUST), fake(runs)), [])

    def test_no_run(self):
        runs = {**GREEN, ("DE0CH/jarvis3", "chat.yml"): []}
        p = c.problems(c.pins(KUST), fake(runs))
        self.assertEqual(len(p), 1)
        self.assertIn("no chat.yml run", p[0])

    def test_still_running(self):
        runs = {**GREEN, ("DE0CH/aviary", "ci.yml"): [("in_progress", None)]}
        p = c.problems(c.pins(KUST), fake(runs))
        self.assertEqual(len(p), 2)  # the fake answers per workflow: both aviary pins (the services', herald's)
        self.assertIn("not finished", p[0])

    def test_real_kustomization_parses(self):
        k = open(os.path.join(os.path.dirname(__file__), "..", "deploy", "kustomization.yaml")).read()
        self.assertEqual([r for r, _, _ in c.pins(k)], ["DE0CH/aviary", "DE0CH/aviary", "DE0CH/claudecodeui",
                                                        "DE0CH/jarvis3", "DE0CH/yyc-drive"])


if __name__ == "__main__":
    unittest.main()

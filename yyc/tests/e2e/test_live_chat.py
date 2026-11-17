#!/usr/bin/env python3
"""live_chat.py's pieces that need no browser (stdlib only): waiting for the e-mailed code, sending once the
app takes a send, telling a reply from the echoed prompt, and reading the names off a `/` list."""
import os, sys, unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import live_chat as e  # noqa: E402


class Code(unittest.TestCase):
    def test_waits_until_the_code_lands(self):
        answers = iter([(404, {"error": "none_yet"}), (404, {"error": "none_yet"}), (200, {"code": "123456"})])
        seen = []
        got = e.wait_code("2026-11-11T03:00:00Z", lambda after: (seen.append(after), next(answers))[1], sleep=lambda s: None)
        self.assertEqual(got, "123456")
        self.assertEqual(seen, ["2026-11-11T03:00:00Z"] * 3)

    def test_gives_up(self):
        with self.assertRaises(SystemExit):
            e.wait_code("2026-11-11T03:00:00Z", lambda a: (404, {"error": "none_yet"}), sleep=lambda s: None, tries=3)

    def test_route_error_is_fatal_at_once(self):
        calls = []
        with self.assertRaises(SystemExit):
            e.wait_code("2026-11-11T03:00:00Z", lambda a: (calls.append(a), (403, {"error": "x"}))[1], sleep=lambda s: None)
        self.assertEqual(len(calls), 1)


class Send(unittest.TestCase):
    def test_presses_until_the_message_has_left(self):
        # the app takes typing before it is ready to send: the first two Enters are not sends
        presses = []
        self.assertTrue(e.send(lambda: presses.append(1), lambda: len(presses) >= 3, wait=lambda: None))
        self.assertEqual(len(presses), 3)

    def test_one_press_when_the_app_is_ready(self):
        presses = []
        self.assertTrue(e.send(lambda: presses.append(1), lambda: True, wait=lambda: None))
        self.assertEqual(len(presses), 1)

    def test_gives_up(self):
        presses = []
        self.assertFalse(e.send(lambda: presses.append(1), lambda: False, wait=lambda: None, tries=4))
        self.assertEqual(len(presses), 4)


class Reply(unittest.TestCase):
    def test_prompt_alone_is_no_reply(self):
        self.assertFalse(e.answered(f"新對話\n{e.PROMPT}\n", e.PROMPT, e.EXPECT))

    def test_reply_after_the_prompt(self):
        self.assertTrue(e.answered(f"新對話\n{e.PROMPT}\n{e.EXPECT}\n", e.PROMPT, e.EXPECT))

    def test_expected_number_is_not_in_the_prompt(self):
        self.assertNotIn(e.EXPECT, e.PROMPT)


class Commands(unittest.TestCase):
    def test_names_of_the_rows_once_each(self):
        # the highlighted command's card, then the rows (the app's own text for a list of three)
        text = "/compact\nSummarise the conversation\n/retire-session\nEnd this session\n/context\n/compact\nSummarise…"
        self.assertEqual(e.command_names(text), ["/compact", "/retire-session", "/context"])

    def test_no_names_in_an_empty_or_loading_list(self):
        self.assertEqual(e.command_names(""), [])
        self.assertEqual(e.command_names("載入中…\n沒有指令\n/"), [])

    def test_a_sentence_with_a_slash_is_no_name(self):
        self.assertEqual(e.command_names("/compact keeps a summary\nsee a/b"), [])


if __name__ == "__main__":
    unittest.main()

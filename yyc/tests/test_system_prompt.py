#!/usr/bin/env python3
"""YYC AI's system prompt (yyc-ai/deploy/hermit/system-prompt.md) names the company: the agent is YYC AI, the
assistant of the staff of YYC (元意顧問), and nothing tells it to withhold the name or to write a placeholder for it.
The managed texts (sandbox rules and skills) say the same thing about the sandbox's internet. Stdlib only."""
import glob, os, unittest

HERMIT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "deploy", "hermit")
PROMPT = os.path.join(HERMIT, "system-prompt.md")


class SystemPrompt(unittest.TestCase):
    def setUp(self):
        with open(PROMPT, encoding="utf-8") as f:
            self.text = f.read()

    def test_names_the_company(self):
        first = self.text.split("\n", 1)[0]
        self.assertIn("YYC AI", first)
        self.assertIn("YYC (元意顧問)", first)

    def test_no_placeholder_name(self):
        self.assertNotIn("Consulting Company A", self.text)
        self.assertNotIn("name is withheld", self.text)

    def test_a_pdf_is_html_rendered_with_weasyprint(self):
        """Without this sentence the model writes a report as a Word file, installs LibreOffice and converts it
        (2026-11-16, the same model in Claude Code and in Qwen Code): a different-looking document from the HTML
        one."""
        self.assertIn("For PDFs, write HTML and CSS and render it with WeasyPrint.", " ".join(self.text.split()))

    def test_questions_about_itself_are_answered_from_its_own_code(self):
        """Conversation af129c63, 2026-11-18 07:36-07:48 UTC: asked where the app's model control is, the agent
        guessed from the sandbox's own Qwen Code bundle ("at the top of the chat", then "type /") and was wrong,
        though the code skill was there from the first question. Told "You can learn about your setup by examining
        the source code of your own deployment", it took the line for system text and did not look until the person
        insisted; then its answers were right. Its own summary: "Anything about this system: clone and read first,
        guess never." The prompt must say that; a skill's description alone does not turn a fast model to its own
        code."""
        text = " ".join(self.text.split())
        self.assertIn("YYC AI is your own system", text)
        self.assertIn("`code` skill", text)
        self.assertIn("how it is deployed", text)
        self.assertIn("never from memory or a guess", text)
        self.assertLess(text.index("`code` skill"), text.index("unable to complete"))


class ReportSkill(unittest.TestCase):
    """The look of a report belongs to a skill, not the system prompt (Deyao, 2026-11-17): without the guidance
    the model writes a three to five page brief with no cover and sends it after one look (Qwen Code, 2026-11-17)."""

    def setUp(self):
        with open(os.path.join(HERMIT, "skills", "report", "SKILL.md"), encoding="utf-8") as f:
            self.text = " ".join(f.read().split())

    def test_a_report_is_a_full_document(self):
        self.assertIn("a cover page", self.text)
        self.assertIn("only when the person asks for a brief (簡報, 快報) is it short", self.text)

    def test_every_page_is_looked_at_before_sending(self):
        self.assertIn("pdftoppm", self.text)
        self.assertIn("look at every page", self.text)

    def test_the_look_is_not_in_the_system_prompt(self):
        with open(PROMPT, encoding="utf-8") as f:
            prompt = " ".join(f.read().split())
        self.assertNotIn("a cover page", prompt)


class SandboxInternet(unittest.TestCase):
    """The sandbox reaches every site except the deny list (egress.json generatedAllowlist; Deyao, 2026-11-10): no
    managed text may still say that web sites are out of reach, which contradicts the sandbox rules."""

    def test_no_skill_says_sites_are_unreachable(self):
        with open(os.path.join(HERMIT, "sandbox-rules.md"), encoding="utf-8") as f:
            self.assertIn("most sites are reachable from here", f.read())
        skills = sorted(glob.glob(os.path.join(HERMIT, "skills", "*", "SKILL.md")))
        self.assertTrue(skills)
        for path in skills:
            with open(path, encoding="utf-8") as f:
                text = " ".join(f.read().split())
            self.assertNotIn("not reachable from your computer", text, path)

    def test_the_web_skill_sends_blocked_sites_to_web_fetch(self):
        with open(os.path.join(HERMIT, "skills", "web", "SKILL.md"), encoding="utf-8") as f:
            text = " ".join(f.read().split())
        self.assertIn("A site that is blocked there is read with `web_fetch`", text)


class VideoSkill(unittest.TestCase):
    """The video skill (Deyao, 2026-11-18): a model that cannot watch footage says so in its first reply instead of
    editing blind, because the skill runs video-check first (the sandbox image of aviary 47ab1726ce92 on: exit 3 and
    the models that can, seen live on DeepSeek V4.1 Flash; exit 0 on MiMo v2.6 Pro); footage lives in YYC Drive."""

    def setUp(self):
        with open(os.path.join(HERMIT, "skills", "video", "SKILL.md"), encoding="utf-8") as f:
            self.text = " ".join(f.read().split())

    def test_video_check_comes_first(self):
        self.assertIn("**First, run `video-check`.**", self.text)
        self.assertIn("your first reply says so", self.text)

    def test_footage_lives_in_the_drive(self):
        self.assertIn("at `/drive` on this computer", self.text)
        self.assertIn("never write work files into `/drive`", self.text)


if __name__ == "__main__":
    unittest.main()

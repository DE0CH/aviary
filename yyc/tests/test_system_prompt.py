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


if __name__ == "__main__":
    unittest.main()

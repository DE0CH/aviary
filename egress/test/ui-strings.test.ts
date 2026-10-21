// The UI's empty new-conversation screen (chat/ui/src): logo, heading, lead and the composer only.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const src = (f: string) => readFileSync(new URL(`../ui/src/${f}`, import.meta.url), "utf8");

test("the empty new-conversation screen has no example prompts (Deyao, 2026-10-21)", () => {
  assert.match(src("i18n.ts"), /emptyTitle:/);
  assert.doesNotMatch(src("i18n.ts"), /: \[/, "no prompt lists in the string tables");
  for (const f of ["i18n.ts", "app.tsx", "style.css"]) assert.doesNotMatch(src(f), /examples/, f);
});

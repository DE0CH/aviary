// The UI's empty new-conversation screen (chat/ui/src): logo, greeting and the composer only.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { pickFirstName } from "../src/common/access.ts";
import { GREETINGS, greeting, slotAt } from "../ui/src/greeting.ts";

const src = (f: string) => readFileSync(new URL(`../ui/src/${f}`, import.meta.url), "utf8");

test("the empty new-conversation screen has no example prompts (Deyao, 2026-10-21)", () => {
  assert.doesNotMatch(src("i18n.ts"), /: \[/, "no prompt lists in the string tables");
  for (const f of ["i18n.ts", "app.tsx", "style.css"]) assert.doesNotMatch(src(f), /examples/, f);
});

test("no description line and no old heading: the greeting replaces them (Deyao, 2026-10-21)", () => {
  const i18n = src("i18n.ts");
  for (const gone of ["emptyLead", "emptyTitle", "今日想做啲咩", "可以搜尋新聞和社交平台"]) assert.ok(!i18n.includes(gone), gone);
  assert.match(src("app.tsx"), /<h2>\{greeting\(locale\(\), new Date\(\), greetingDraw, me\.firstName\)\}<\/h2>/);
});

test("greetings come from the cited published lists, by Hong Kong time of day", () => {
  const g = src("greeting.ts");
  for (const url of ["https://en.wikivoyage.org/wiki/Cantonese_phrasebook", "https://en.wiktionary.org/wiki/Category:English_greetings"]) assert.ok(g.includes(url), url);
  // Hong Kong is UTC+8 all year
  const at = (hkHour: number) => new Date(Date.UTC(2026, 9, 21, (hkHour - 8 + 24) % 24, 30));
  assert.equal(slotAt(at(5)), "morning");
  assert.equal(slotAt(at(11)), "morning");
  assert.equal(slotAt(at(12)), "afternoon");
  assert.equal(slotAt(at(18)), "evening");
  assert.equal(slotAt(at(23)), "night");
  assert.equal(slotAt(at(2)), "night");
  assert.equal(greeting("zh-HK", at(8), 0), "早晨");
  assert.equal(greeting("en", at(8), 0, "Deyao"), "Good morning, Deyao");
  assert.equal(greeting("zh-HK", at(8), 0, "Deyao"), "早晨，Deyao");
  assert.equal(greeting("zh-HK", at(20), 0.99, "Deyao"), "食飽未，Deyao？");
  assert.equal(greeting("en", at(20), 0.99, "  "), "How's everything?");
  for (const loc of ["zh-HK", "en"] as const) for (const [slot, list] of Object.entries(GREETINGS[loc])) {
    assert.ok(list.length >= 2, `${loc} ${slot}`);
    for (let i = 0; i < list.length; i++) assert.ok(list.includes(greeting(loc, at({ morning: 8, afternoon: 14, evening: 20, night: 1 }[slot]!), i / list.length)));
  }
});

test("the greeting's name: the identity's first name, never the address", () => {
  assert.equal(pickFirstName({ given_name: "Deyao", name: "Deyao Chen" }), "Deyao");
  assert.equal(pickFirstName({ name: "Deyao Chen" }), "Deyao");
  assert.equal(pickFirstName({ name: "陳大文" }), "陳大文");
  assert.equal(pickFirstName({ name: "a@b.org" }), null);
  assert.equal(pickFirstName({}), null);
  assert.equal(pickFirstName(null), null);
});

test("firstName from the Access identity (get-identity), cached; null when the identity has none", async () => {
  const { AccessVerifier } = await import("../src/common/access.ts");
  const { fakeAccess, TEAM, AUD } = await import("./fakes/access.ts");
  const access = await fakeAccess();
  let calls = 0;
  const v = new AccessVerifier({ teamDomain: TEAM, audience: AUD, jwks: access.jwks, identity: async () => (calls++, { name: "Tai Man Chan" }) });
  const tok = await access.token("a@example.org");
  assert.equal(await v.firstName(tok, "a@example.org"), "Tai");
  assert.equal(await v.firstName(tok, "a@example.org"), "Tai");
  assert.equal(calls, 1);
  const none = new AccessVerifier({ teamDomain: TEAM, audience: AUD, jwks: access.jwks, identity: async () => ({ email: "b@example.org" }) });
  assert.equal(await none.firstName(await access.token("b@example.org"), "b@example.org"), null);
  assert.equal(await none.firstName("garbage", "c@example.org"), null);
});

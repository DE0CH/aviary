// The UI's empty new-conversation screen (chat/ui/src): logo, greeting and the composer only.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { pickFirstName } from "../src/common/access.ts";
import { greeting, pool, slotAt } from "../ui/src/greeting.ts";

const src = (f: string) => readFileSync(new URL(`../ui/src/${f}`, import.meta.url), "utf8");

test("the empty new-conversation screen has no example prompts (Deyao, 2026-10-21)", () => {
  assert.doesNotMatch(src("i18n.ts"), /: \[/, "no prompt lists in the string tables");
  for (const f of ["i18n.ts", "app.tsx", "style.css"]) assert.doesNotMatch(src(f), /examples/, f);
});

test("no description line and no old heading: the greeting replaces them (Deyao, 2026-10-21)", () => {
  const i18n = src("i18n.ts");
  for (const gone of ["emptyLead", "emptyTitle", "今日想做啲咩", "可以搜尋新聞和社交平台"]) assert.ok(!i18n.includes(gone), gone);
  assert.match(src("app.tsx"), /greeting\(locale\(\), new Date\(\), greetingDraw, me\.firstName, returning\)/);
});

// Hong Kong is UTC+8 all year
const at = (hkHour: number) => new Date(Date.UTC(2026, 9, 21, (hkHour - 8 + 24) % 24, 30));
const HOUR = { morning: 8, afternoon: 14, evening: 20, night: 1 } as const;
const ZH_ANY = ["你好呀", "你好", "哈囉", "嗨", "喂，你好", "哈囉哈囉", "嘿", "你好嗎？", "點呀？", "近排點呀？", "一切順利嗎？", "最近過成點？", "有咩新嘢？", "而家發生緊咩事？"];
const EN_ANY = ["Good day", "Hello", "Hi", "Hey", "Hello there", "Hiya", "Hey there", "How are you?", "How are you doing?", "How's it going?", "How's everything?", "How's life?", "What's new?", "What's going on?"];

test("the approved phrase set (Deyao, 2026-10-21): sources as stated, old sourcing claims gone", () => {
  const g = src("greeting.ts");
  assert.ok(g.includes("https://en.wiktionary.org/wiki/Category:English_greetings"));
  assert.match(g, /translated by Claude in the session and approved by Deyao/);
  for (const gone of ["wikivoyage", "Wikivoyage", "午安", "晚上好", "食飽未", "晚安", "CantoneseClass101", "有咩新聞"]) assert.ok(!g.includes(gone), gone);
});

test("the Hong Kong time slots", () => {
  assert.equal(slotAt(at(5)), "morning");
  assert.equal(slotAt(at(11)), "morning");
  assert.equal(slotAt(at(12)), "afternoon");
  assert.equal(slotAt(at(17)), "afternoon");
  assert.equal(slotAt(at(18)), "evening");
  assert.equal(slotAt(at(22)), "evening");
  assert.equal(slotAt(at(23)), "night");
  assert.equal(slotAt(at(2)), "night");
  assert.equal(slotAt(at(4)), "night");
});

test("each slot's pool: its own phrases, the any-time ones, the daytime one in the morning and afternoon", () => {
  const DAY_ZH = ["今日打得掂嗎？"], DAY_EN = ["How goes the battle?"];
  assert.deepEqual(pool("zh-HK", "morning"), ["早晨", "早呀", "早晨！今日精神嗎？", ...ZH_ANY, ...DAY_ZH]);
  assert.deepEqual(pool("zh-HK", "afternoon"), ["下晝好", "食咗飯未呀？", ...ZH_ANY, ...DAY_ZH]);
  assert.deepEqual(pool("zh-HK", "evening"), ["夜晚好", "食咗飯未呀？", ...ZH_ANY]);
  assert.deepEqual(pool("zh-HK", "night"), ["夜喇，仲未瞓？", "早唞", ...ZH_ANY]);
  assert.deepEqual(pool("en", "morning"), ["Good morning", "Morning", "Top of the morning", ...EN_ANY, ...DAY_EN]);
  assert.deepEqual(pool("en", "afternoon"), ["Good afternoon", "How's it going?", ...EN_ANY, ...DAY_EN]);
  assert.deepEqual(pool("en", "evening"), ["Good evening", "How's everything?", ...EN_ANY]);
  assert.deepEqual(pool("en", "night"), ["Hello again", "Hi there", ...EN_ANY]);
  for (const slot of ["morning", "afternoon", "evening", "night"] as const) {
    // the en list is the zh-HK list's counterpart, phrase for phrase; each locale draws only from its own
    assert.equal(pool("en", slot, true).length, pool("zh-HK", slot, true).length, slot);
    for (const loc of ["zh-HK", "en"] as const) {
      const list = pool(loc, slot);
      const other = pool(loc === "en" ? "zh-HK" : "en", slot, true);
      const seen = new Set<string>();
      for (let i = 0; i < list.length; i++) {
        const g = greeting(loc, at(HOUR[slot]), i / list.length);
        assert.ok(list.includes(g), `${loc} ${slot} ${g}`);
        assert.ok(!other.includes(g), `${loc} ${slot} ${g} is the other locale's`);
        seen.add(g);
      }
      assert.deepEqual([...seen].sort(), [...new Set(list)].sort(), `${loc} ${slot}: every phrase reachable`);
      assert.equal(greeting(loc, at(HOUR[slot]), 0.999999), list.at(-1));
    }
  }
  for (const gone of ["有咩新聞？", "午安", "晚上好"]) for (const slot of ["morning", "afternoon", "evening", "night"] as const) assert.ok(!pool("zh-HK", slot, true).includes(gone), gone);
});

test("returning users only: 又見面喇 / 歡迎返嚟 (Hello again / Welcome) join the pool with earlier conversations", () => {
  for (const slot of ["morning", "afternoon", "evening", "night"] as const) {
    assert.ok(!pool("zh-HK", slot).includes("又見面喇") && !pool("zh-HK", slot).includes("歡迎返嚟"), slot);
    assert.ok(!pool("en", slot).includes("Welcome"), slot);
    assert.deepEqual(pool("zh-HK", slot, true), [...pool("zh-HK", slot), "又見面喇", "歡迎返嚟"]);
    assert.deepEqual(pool("en", slot, true), [...pool("en", slot), "Hello again", "Welcome"]);
    // the last draw lands on a returning phrase only for a returning user
    assert.equal(greeting("zh-HK", at(HOUR[slot]), 0.999999, null, true), "歡迎返嚟");
    assert.notEqual(greeting("zh-HK", at(HOUR[slot]), 0.999999, null, false), "歡迎返嚟");
  }
  assert.equal(greeting("en", at(20), 0.999999, "Deyao", true), "Welcome, Deyao");
});

test("the name: after a comma, before a closing question mark", () => {
  assert.equal(greeting("zh-HK", at(8), 0), "早晨");
  assert.equal(greeting("en", at(8), 0, "Deyao"), "Good morning, Deyao");
  assert.equal(greeting("zh-HK", at(8), 0, "Deyao"), "早晨，Deyao");
  assert.equal(greeting("en", at(8), 0, "  "), "Good morning");
  const idx = (loc: "zh-HK" | "en", slot: keyof typeof HOUR, phrase: string) => {
    const list = pool(loc, slot);
    return (list.indexOf(phrase) + 0.5) / list.length;
  };
  assert.equal(greeting("zh-HK", at(14), idx("zh-HK", "afternoon", "點呀？"), "Tai Man"), "點呀，Tai Man？");
  assert.equal(greeting("zh-HK", at(8), idx("zh-HK", "morning", "早晨！今日精神嗎？"), "Tai Man"), "早晨！今日精神嗎，Tai Man？");
  assert.equal(greeting("zh-HK", at(1), idx("zh-HK", "night", "夜喇，仲未瞓？"), "陳大文"), "夜喇，仲未瞓，陳大文？");
  assert.equal(greeting("zh-HK", at(20), idx("zh-HK", "evening", "食咗飯未呀？"), "Deyao"), "食咗飯未呀，Deyao？");
  assert.equal(greeting("en", at(20), idx("en", "evening", "How are you?"), "Deyao"), "How are you, Deyao?");
  assert.equal(greeting("en", at(14), idx("en", "afternoon", "How goes the battle?"), "Deyao"), "How goes the battle, Deyao?");
  assert.equal(greeting("en", at(1), idx("en", "night", "Hi there"), "Deyao"), "Hi there, Deyao");
  assert.equal(greeting("zh-HK", at(1), idx("zh-HK", "night", "早唞"), "Deyao"), "早唞，Deyao");
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

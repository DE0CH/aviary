import assert from "node:assert/strict";
import { test } from "node:test";
import { BodyBudget, FORCED_PROVIDER, forceZdr, topLevelMembers } from "../src/egress/modelbody.ts";

const out = (raw: string | Buffer, plan?: Parameters<typeof forceZdr>[1]) => {
  const r = forceZdr(Buffer.isBuffer(raw) ? raw : Buffer.from(raw), plan);
  if (!r) return null;
  const whole = Buffer.concat(r.parts);
  assert.equal(whole.length, r.length);
  return JSON.parse(whole.toString("utf8")) as Record<string, unknown>;
};

/** What the rewrite must equal: the body through JSON.parse, provider merged, thinking applied (the old object-based
 * forceZdr, kept here as the reference). */
function reference(raw: string, plan?: Parameters<typeof forceZdr>[1]): Record<string, unknown> | null {
  let b: unknown;
  try {
    b = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!b || typeof b !== "object" || Array.isArray(b)) return null;
  const o = b as Record<string, unknown>;
  const p = o.provider && typeof o.provider === "object" && !Array.isArray(o.provider) ? o.provider : {};
  o.provider = { ...p, ...FORCED_PROVIDER };
  if (plan && typeof o.model === "string") {
    const bare = (m: string) => m.slice(m.lastIndexOf("/") + 1);
    const model = o.model;
    const entry = plan[model] ?? Object.entries(plan).find(([id]) => bare(id) === model)?.[1];
    const effort = Array.isArray(o.tools) && o.tools.length ? entry?.withTools : entry?.withoutTools;
    if (effort === "none") {
      o.thinking = { type: "disabled" };
      delete o.reasoning;
    } else if (effort) o.reasoning = { effort };
  }
  return o;
}

const PLAN = { "deepseek/deepseek-v4.1-flash-1017": { withTools: "high" as const, withoutTools: "none" as const } };

test("forceZdr: provider options merged over the caller's, non-objects refused", () => {
  for (const bad of ["[]", "{", "", "null", "1", '"x"', "{} {}", '{"a":1,}', "{'a':1}", '{"a":01}', '{"a":1.}', '{"a":"\\x"}', '{"a":"\t"}'])
    assert.equal(forceZdr(Buffer.from(bad)), null, `${JSON.stringify(bad)} must be refused`);
  assert.deepEqual(out('{"model":"m","provider":{"zdr":false,"only":["x"]}}'), { model: "m", provider: { zdr: true, data_collection: "deny", only: ["x"] } });
  assert.deepEqual(out("{}"), { provider: FORCED_PROVIDER });
  assert.deepEqual(out(' \n{ "provider" : "anything" } \r\n'), { provider: FORCED_PROVIDER });
  assert.deepEqual(FORCED_PROVIDER, { zdr: true, data_collection: "deny" });
});

test("forceZdr: duplicate and escaped keys can't smuggle a provider past it", () => {
  // JSON.parse keeps the last duplicate; every occurrence is dropped and the merged one appended
  assert.deepEqual(out('{"provider":{"zdr":true},"provider":{"zdr":false,"order":["a"]}}'), { provider: { zdr: true, data_collection: "deny", order: ["a"] } });
  const esc = '{"\\u0070rovider":{"zdr":false},"provider":{"data_collection":"allow"},"pro\\u0076ider":{"only":["b"]}}';
  assert.deepEqual(out(esc), { provider: { zdr: true, data_collection: "deny", only: ["b"] } });
  const r = forceZdr(Buffer.from(esc))!;
  assert.equal((Buffer.concat(r.parts).toString().match(/provider|\\u0070|\\u0076/g) ?? []).length, 1);
  // a nested "provider" is data, not the option
  assert.deepEqual(out('{"messages":[{"provider":{"zdr":false}}]}'), { messages: [{ provider: { zdr: false } }], provider: FORCED_PROVIDER });
});

test("forceZdr: the model's thinking level, by whether the request carries tools", () => {
  const run = (body: object) => out(JSON.stringify(body), PLAN)!;
  const chat = run({ model: "deepseek/deepseek-v4.1-flash-1017", tools: [{ name: "Bash" }], thinking: { type: "enabled", budget_tokens: 9 } });
  assert.deepEqual(chat.reasoning, { effort: "high" });
  assert.deepEqual(chat.thinking, { type: "enabled", budget_tokens: 9 });
  const title = run({ model: "deepseek/deepseek-v4.1-flash-1017", reasoning: { effort: "max" } });
  assert.deepEqual(title.thinking, { type: "disabled" });
  assert.equal(title.reasoning, undefined);
  assert.deepEqual(run({ model: "deepseek/deepseek-v4.1-flash-1017", tools: [] }).thinking, { type: "disabled" });
  assert.deepEqual(out('{"model":"deepseek/deepseek-v4.1-flash-1017","tools":[ \n ]}', PLAN)!.thinking, { type: "disabled" });
  // bare ids match, other models pass as sent
  assert.deepEqual(run({ model: "deepseek-v4.1-flash-1017", tools: [{ name: "x" }] }).reasoning, { effort: "high" });
  const other = run({ model: "anthropic/claude-sonnet-5.5", tools: [{ name: "x" }] });
  assert.equal(other.reasoning, undefined);
  assert.equal(other.thinking, undefined);
  assert.equal(forceZdr(Buffer.from('{"model":"m"}'))!.model, "m");
  assert.equal(forceZdr(Buffer.from('{"a":1}'))!.model, undefined);
  assert.deepEqual(forceZdr(Buffer.from('{"model":["x"]}'))!.model, ["x"]);
});

test("forceZdr: UTF-8 is checked, so no reader can disagree about where a string ends", () => {
  const ok = Buffer.from('{"t":"特首新聞摘要 😀 é"}');
  assert.deepEqual(out(ok), { t: "特首新聞摘要 😀 é", provider: FORCED_PROVIDER });
  const bad = (bytes: number[]) => Buffer.concat([Buffer.from('{"t":"'), Buffer.from(bytes), Buffer.from('"}')]);
  for (const seq of [[0xc0, 0x80], [0xc1, 0xbf], [0xe0, 0x80, 0x80], [0xed, 0xa0, 0x80], [0xf0, 0x80, 0x80, 0x80], [0xf4, 0x90, 0x80, 0x80], [0xf5, 0x80, 0x80, 0x80], [0x80], [0xe4, 0xb8], [0xc3, 0x22]])
    assert.equal(forceZdr(bad(seq)), null, `bytes ${seq.map((x) => x.toString(16))} must be refused`);
  assert.equal(forceZdr(Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from("{}")])), null, "a BOM is refused, as JSON.parse refuses it");
});

test("forceZdr: accepts exactly what JSON.parse accepts and rewrites to the same object (fuzzed)", () => {
  let seed = 12345;
  const rnd = (n: number) => {
    seed = (seed * 1103515245 + 12345) % 2 ** 31;
    return seed % n;
  };
  const ws = () => [" ", "\n", "\t", "\r", ""][rnd(5)]!.repeat(rnd(3));
  const str = () => JSON.stringify(["provider", "model", "tools", "reasoning", "thinking", "x", "é ", "\u0000\"\\", "😀"][rnd(9)]).replace(/r/g, () => (rnd(4) ? "r" : "\\u0072"));
  const val = (d: number): string => {
    const k = rnd(d > 3 ? 5 : 8);
    if (k === 0) return String([0, -1, 1.5, -0.25e-3, 1e21, 12][rnd(6)]);
    if (k === 1) return ["true", "false", "null"][rnd(3)]!;
    if (k <= 3) return str();
    if (k === 4) return '"deepseek/deepseek-v4.1-flash-1017"';
    if (k <= 6) return "[" + ws() + Array.from({ length: rnd(4) }, () => ws() + val(d + 1) + ws()).join(",") + "]";
    return "{" + ws() + Array.from({ length: rnd(5) }, () => ws() + str() + ws() + ":" + ws() + val(d + 1) + ws()).join(",") + "}";
  };
  let valid = 0;
  for (let n = 0; n < 4000; n++) {
    let doc = ws() + "{" + Array.from({ length: rnd(6) }, () => ws() + str() + ws() + ":" + ws() + val(0) + ws()).join(",") + "}" + ws();
    if (n % 2) {
      // mutate: replace or insert one character (whole code points: a split surrogate pair is no JSON test)
      const cs = Array.from(doc);
      const i = rnd(cs.length + 1);
      const c = ['"', "\\", ",", ":", "{", "}", "[", "]", "0", "-", ".", "e", "u", " ", ""][rnd(15)]!;
      cs.splice(i, rnd(2), c);
      doc = cs.join("");
    }
    const plan = rnd(2) ? PLAN : undefined;
    const want = reference(doc, plan);
    const got = out(doc, plan);
    assert.deepEqual(got, want, `doc ${JSON.stringify(doc)}`);
    if (want) valid++;
  }
  assert.ok(valid > 2000, `${valid} valid documents in the corpus`);
});

test("forceZdr: a large body costs about its own size: the output is slices of the input", () => {
  const image = "iVBORw0KGgo" + "A".repeat(20 * 1024 * 1024);
  const raw = Buffer.from(JSON.stringify({ model: "m", messages: [{ role: "user", content: [{ type: "image", source: { type: "base64", media_type: "image/png", data: image } }] }], tools: [{ name: "Read" }] }));
  const t0 = process.hrtime.bigint();
  const r = forceZdr(raw, PLAN)!;
  const ms = Number(process.hrtime.bigint() - t0) / 1e6;
  assert.ok(r.length - raw.length < 200);
  const big = r.parts.find((p) => p.length > 1024 * 1024)!;
  assert.equal(big.buffer, raw.buffer, "the large member is a view of the request's own Buffer, not a copy");
  assert.ok(ms < 2000, `scan took ${ms} ms`);
});

test("the scanner: deep nesting needs no stack", () => {
  const depth = 200_000;
  const doc = '{"a":' + "[".repeat(depth) + "]".repeat(depth) + "}";
  assert.equal(topLevelMembers(Buffer.from(doc))?.length, 1);
  assert.equal(topLevelMembers(Buffer.from('{"a":' + "[".repeat(depth) + "]".repeat(depth - 1) + "}")), null);
});

test("BodyBudget: bytes are handed out first come first served, a too-big request runs alone", async () => {
  const b = new BodyBudget(100);
  const r1 = await b.take(60);
  const order: string[] = [];
  const p2 = b.take(50).then((r) => (order.push("50"), r));
  const p3 = b.take(10).then((r) => (order.push("10"), r)); // fits now, but waits its turn behind 50
  await new Promise((r) => setTimeout(r, 10));
  assert.deepEqual([...order], []);
  assert.deepEqual(b.state, { taken: 60, waiting: 2 });
  r1();
  r1(); // a second call returns nothing more
  const [r2, r3] = await Promise.all([p2, p3]);
  assert.deepEqual([...order], ["50", "10"]);
  assert.deepEqual(b.state, { taken: 60, waiting: 0 });
  const p4 = b.take(1000).then((r) => (order.push("1000"), r));
  r2();
  r3();
  const r4 = await p4;
  assert.deepEqual(b.state, { taken: 100, waiting: 0 });
  r4();
  assert.deepEqual(b.state, { taken: 0, waiting: 0 });
});

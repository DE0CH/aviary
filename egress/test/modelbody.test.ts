import assert from "node:assert/strict";
import { test } from "node:test";
import { FORCED_PROVIDER, HELD_MAX, InvalidBody, ModelBodyRewriter } from "../src/egress/modelbody.ts";
import type { ModelPlan } from "../src/common/config.ts";

/** The body through the streaming rewriter in chunks of `chunk` bytes (0: all at once); null when it is refused. */
function rewrite(raw: Buffer, plan?: ModelPlan, chunk = 0): { model: unknown; body: Buffer } | null {
  const rw = new ModelBodyRewriter(plan);
  const parts: Buffer[] = [];
  try {
    for (let i = 0; i < raw.length; i += chunk || raw.length) parts.push(...rw.push(raw.subarray(i, i + (chunk || raw.length))));
    parts.push(rw.end());
  } catch (e) {
    if (e instanceof InvalidBody) return null;
    throw e;
  }
  return { model: rw.model, body: Buffer.concat(parts) };
}

/** The rewritten body as an object; the same whatever the chunking (all at once, byte by byte, 7 bytes at a time). */
const out = (raw: string | Buffer, plan?: ModelPlan) => {
  const b = Buffer.isBuffer(raw) ? raw : Buffer.from(raw);
  const [whole, ...rest] = [0, 1, 7].map((c) => rewrite(b, plan, c));
  for (const r of rest) assert.deepEqual(r && { model: r.model, body: r.body.toString("latin1") }, whole && { model: whole.model, body: whole.body.toString("latin1") }, "chunking changes nothing");
  return whole ? (JSON.parse(whole.body.toString("utf8")) as Record<string, unknown>) : null;
};
const refused = (raw: string | Buffer) => out(raw) === null;

/** What the rewrite must equal: the body through JSON.parse, provider merged, thinking applied (the old object-based
 * rewrite, kept here as the reference). */
function reference(raw: string, plan?: ModelPlan): Record<string, unknown> | null {
  let b: unknown;
  try {
    b = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!b || typeof b !== "object" || Array.isArray(b)) return null;
  const o = b as Record<string, unknown>;
  const p = o.provider && typeof o.provider === "object" && !Array.isArray(o.provider) ? o.provider : {};
  const bare = (m: string) => m.slice(m.lastIndexOf("/") + 1);
  const entry = plan && typeof o.model === "string" ? (plan[o.model] ?? Object.entries(plan).find(([id]) => bare(id) === o.model)?.[1]) : undefined;
  const pin = entry?.providers ? { order: entry.providers, only: entry.providers, allow_fallbacks: false } : {};
  o.provider = { ...p, ...pin, ...FORCED_PROVIDER };
  if (plan && typeof o.model === "string") {
    const effort = Array.isArray(o.tools) && o.tools.length ? entry?.withTools : entry?.withoutTools;
    if (effort === "none") {
      o.thinking = { type: "disabled" };
      delete o.reasoning;
    } else if (effort) o.reasoning = { effort };
  }
  return o;
}

const PLAN = { "deepseek/deepseek-v4.1-flash-1017": { withTools: "high" as const, withoutTools: "none" as const, providers: ["deepinfra/fp8", "morph/fp8"] } };

test("rewrite: a model's pinned providers become order + only with no fallback, over the caller's", () => {
  const pin = { order: ["deepinfra/fp8", "morph/fp8"], only: ["deepinfra/fp8", "morph/fp8"], allow_fallbacks: false };
  const body = { model: "deepseek/deepseek-v4.1-flash-1017", provider: { only: ["open-inference/fp4"], allow_fallbacks: true, sort: "price", zdr: false } };
  assert.deepEqual(out(JSON.stringify(body), PLAN)!.provider, { sort: "price", ...pin, ...FORCED_PROVIDER });
  assert.deepEqual(out('{"model":"deepseek-v4.1-flash-1017"}', PLAN)!.provider, { ...pin, ...FORCED_PROVIDER }, "the bare id too");
  assert.deepEqual(out('{"model":"z-ai/glm-5.3","provider":{"only":["x"]}}', PLAN)!.provider, { only: ["x"], ...FORCED_PROVIDER }, "an unpinned model is left to OpenRouter");
});

test("rewrite: provider options merged over the caller's, non-objects refused", () => {
  for (const bad of ["[]", "{", "", "null", "1", '"x"', "{} {}", '{"a":1,}', "{'a':1}", '{"a":01}', '{"a":1.}', '{"a":"\\x"}', '{"a":"\t"}'])
    assert.ok(refused(bad), `${JSON.stringify(bad)} must be refused`);
  assert.deepEqual(out('{"model":"m","provider":{"zdr":false,"only":["x"]}}'), { model: "m", provider: { zdr: true, data_collection: "deny", only: ["x"] } });
  assert.deepEqual(out("{}"), { provider: FORCED_PROVIDER });
  assert.deepEqual(out(' \n{ "provider" : "anything" } \r\n'), { provider: FORCED_PROVIDER });
  assert.deepEqual(FORCED_PROVIDER, { zdr: true, data_collection: "deny" });
});

test("rewrite: duplicate and escaped keys can't smuggle a provider past it", () => {
  // JSON.parse keeps the last duplicate; every occurrence is dropped and the merged one appended
  assert.deepEqual(out('{"provider":{"zdr":true},"provider":{"zdr":false,"order":["a"]}}'), { provider: { zdr: true, data_collection: "deny", order: ["a"] } });
  const esc = '{"\\u0070rovider":{"zdr":false},"provider":{"data_collection":"allow"},"pro\\u0076ider":{"only":["b"]}}';
  assert.deepEqual(out(esc), { provider: { zdr: true, data_collection: "deny", only: ["b"] } });
  const r = rewrite(Buffer.from(esc))!;
  assert.equal((r.body.toString().match(/provider|\\u0070|\\u0076/g) ?? []).length, 1);
  // a nested "provider" is data, not the option
  assert.deepEqual(out('{"messages":[{"provider":{"zdr":false}}]}'), { messages: [{ provider: { zdr: false } }], provider: FORCED_PROVIDER });
});

test("rewrite: the model's thinking level, by whether the request carries tools", () => {
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
  assert.equal(rewrite(Buffer.from('{"model":"m"}'))!.model, "m");
  assert.equal(rewrite(Buffer.from('{"a":1}'))!.model, undefined);
  assert.deepEqual(rewrite(Buffer.from('{"model":["x"]}'))!.model, ["x"]);
  // the last of duplicate members counts, wherever the members stand
  assert.deepEqual(run({ tools: [{ name: "x" }], thinking: { type: "enabled" }, messages: [], model: "deepseek/deepseek-v4.1-flash-1017" }).reasoning, { effort: "high" });
  assert.equal(out('{"model":"a","x":1,"model":"b"}')!.model, "b");
  assert.deepEqual(out('{"model":"deepseek/deepseek-v4.1-flash-1017","tools":[{"name":"x"}],"tools":[]}', PLAN)!.thinking, { type: "disabled" });
});

test("rewrite: UTF-8 is checked, so no reader can disagree about where a string ends", () => {
  const ok = Buffer.from('{"t":"特首新聞摘要 😀 é"}');
  assert.deepEqual(out(ok), { t: "特首新聞摘要 😀 é", provider: FORCED_PROVIDER });
  const bad = (bytes: number[]) => Buffer.concat([Buffer.from('{"t":"'), Buffer.from(bytes), Buffer.from('"}')]);
  for (const seq of [[0xc0, 0x80], [0xc1, 0xbf], [0xe0, 0x80, 0x80], [0xed, 0xa0, 0x80], [0xf0, 0x80, 0x80, 0x80], [0xf4, 0x90, 0x80, 0x80], [0xf5, 0x80, 0x80, 0x80], [0x80], [0xe4, 0xb8], [0xc3, 0x22]])
    assert.ok(refused(bad(seq)), `bytes ${seq.map((x) => x.toString(16))} must be refused`);
  assert.ok(refused(Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), Buffer.from("{}")])), "a BOM is refused, as JSON.parse refuses it");
});

test("rewrite: accepts exactly what JSON.parse accepts and rewrites to the same object (fuzzed)", () => {
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

test("rewrite: a large body passes as views of its chunks; only the small settings members are held", () => {
  const image = "iVBORw0KGgo" + "A".repeat(20 * 1024 * 1024);
  const raw = Buffer.from(JSON.stringify({ model: "deepseek/deepseek-v4.1-flash-1017", messages: [{ role: "user", content: [{ type: "image", source: { type: "base64", media_type: "image/png", data: image } }] }], tools: [{ name: "Read" }], thinking: { type: "enabled" } }));
  const rw = new ModelBodyRewriter(PLAN);
  const t0 = process.hrtime.bigint();
  let outBytes = 0;
  let copied = 0;
  for (let i = 0; i < raw.length; i += 65536) {
    for (const p of rw.push(raw.subarray(i, i + 65536))) {
      outBytes += p.length;
      if (p.buffer !== raw.buffer) copied += p.length;
    }
    if (i === 0) assert.equal(rw.model, "deepseek/deepseek-v4.1-flash-1017", "the model is known from the first chunk, before the conversation");
  }
  const tail = rw.end();
  const ms = Number(process.hrtime.bigint() - t0) / 1e6;
  assert.ok(copied < 64, `${copied} bytes of the passed members were copied`);
  assert.ok(tail.length < 400, "the tail is the held members and the forced ones");
  assert.ok(Math.abs(outBytes + tail.length - raw.length) < 300);
  assert.deepEqual(JSON.parse(tail.toString().replace(/^,/, "{")), { model: "deepseek/deepseek-v4.1-flash-1017", thinking: { type: "enabled" }, provider: { order: ["deepinfra/fp8", "morph/fp8"], only: ["deepinfra/fp8", "morph/fp8"], allow_fallbacks: false, ...FORCED_PROVIDER }, reasoning: { effort: "high" } });
  assert.ok(ms < 2000, `scan took ${ms} ms`);
});

test("rewrite: a held member past its limit is refused, a long key is passed on", () => {
  assert.ok(refused(`{"provider":{"x":"${"a".repeat(HELD_MAX)}"}}`));
  assert.ok(!refused(`{"thinking":{"x":"${"a".repeat(HELD_MAX - 100)}"},"thinking":{"x":"${"a".repeat(HELD_MAX - 100)}"}}`), "a replaced duplicate no longer counts");
  const long = "k".repeat(5000);
  assert.deepEqual(out(`{"a":1,"${long}":2,"model":"m"}`), { a: 1, [long]: 2, model: "m", provider: FORCED_PROVIDER });
});

test("the scanner: deep nesting needs no stack", () => {
  const depth = 200_000;
  assert.ok(rewrite(Buffer.from('{"a":' + "[".repeat(depth) + "]".repeat(depth) + "}")));
  assert.equal(rewrite(Buffer.from('{"a":' + "[".repeat(depth) + "]".repeat(depth - 1) + "}")), null);
});

test("rewrite: a model with thinking levels takes the level the caller asks for, within what it offers", () => {
  const plan: ModelPlan = {
    "deepseek/deepseek-v4.1-flash-1017": { withTools: "high", withoutTools: "none", levels: ["none", "low", "high", "max"] },
    "z-ai/glm-5.3": { withTools: "high", levels: ["low", "high", "max"] },
  };
  const tools = [{ name: "Bash" }];
  const turn = (model: string, extra: object) => out(JSON.stringify({ model, tools, ...extra }), plan)!;
  const D = "deepseek/deepseek-v4.1-flash-1017";
  // Claude Code's request: `thinking` adaptive and `output_config.effort` (its effort level), or no `thinking` at all
  // when thinking is switched off
  const on = { type: "adaptive", display: "updates" };
  for (const [effort, want] of [["low", "low"], ["high", "high"], ["max", "max"], ["medium", "low"], ["xhigh", "high"]] as const) {
    const r = turn(D, { thinking: on, output_config: { effort } });
    assert.deepEqual(r.reasoning, { effort: want }, `effort ${effort}`);
    assert.deepEqual(r.thinking, on);
    assert.deepEqual(r.output_config, { effort }, "the caller's output_config passes as sent");
  }
  // no level named: the model's own (`reasoning`)
  assert.deepEqual(turn(D, { thinking: on }).reasoning, { effort: "high" });
  assert.deepEqual(turn(D, { thinking: on, output_config: { effort: "nonsense" } }).reasoning, { effort: "high" });
  // thinking off
  for (const off of [{}, { thinking: { type: "disabled" } }, { output_config: { effort: "max" } }]) {
    const r = turn(D, off);
    assert.deepEqual(r.thinking, { type: "disabled" }, JSON.stringify(off));
    assert.equal(r.reasoning, undefined);
  }
  // a model that cannot switch thinking off takes its lowest level instead
  const g = turn("z-ai/glm-5.3", { output_config: { effort: "high" } });
  assert.deepEqual(g.reasoning, { effort: "low" });
  assert.equal(g.thinking, undefined);
  assert.deepEqual(turn("z-ai/glm-5.3", { thinking: on, output_config: { effort: "max" } }).reasoning, { effort: "max" });
  // the caller's own `reasoning` never counts, and a call without tools keeps the plan's level
  assert.deepEqual(turn(D, { thinking: on, output_config: { effort: "low" }, reasoning: { effort: "max" } }).reasoning, { effort: "low" });
  const title = out(JSON.stringify({ model: D, thinking: on, output_config: { effort: "max" } }), plan)!;
  assert.deepEqual(title.thinking, { type: "disabled" });
  // a model without levels is as before: the plan's level whatever the caller asks
  const fixed = out(JSON.stringify({ model: D, tools, thinking: on, output_config: { effort: "low" } }), PLAN)!;
  assert.deepEqual(fixed.reasoning, { effort: "high" });
});

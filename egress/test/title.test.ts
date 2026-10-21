import assert from "node:assert/strict";
import { test } from "node:test";
import { cleanTitle, fallbackTitle, TITLE_MAX, titleFromResponse, titleRequest } from "../src/web/title.ts";

test("the title request thinks at the smallest budget and leaves room for the title", () => {
  const b = titleRequest("anthropic/claude-sonnet-5.5", "x".repeat(5000), "y".repeat(5000)) as any;
  assert.equal(b.model, "anthropic/claude-sonnet-5.5");
  // Sonnet 5.5 on OpenRouter: "Reasoning is mandatory for this endpoint and cannot be disabled."
  assert.deepEqual(b.thinking, { type: "enabled", budget_tokens: 1024 });
  assert.ok(b.max_tokens >= b.thinking.budget_tokens + 500);
  assert.ok(b.messages[0].content.length < 3100);
  assert.equal(b.stream, undefined);
});

test("the title is the final text, thinking ignored, cleaned and cut", () => {
  assert.equal(titleFromResponse({ content: [{ type: "thinking", thinking: "hmm" }, { type: "text", text: " 「供應商延遲交貨電郵」\n" }] }), "供應商延遲交貨電郵");
  assert.equal(titleFromResponse({ content: [{ type: "text", text: "draft" }, { type: "text", text: '"Final  title"' }] }), "Final title");
  assert.equal(titleFromResponse({ content: [{ type: "thinking", thinking: "only thinking" }] }), "");
  assert.equal(titleFromResponse({ content: [{ type: "redacted_thinking", data: "x" }, { type: "text", text: "  " }] }), "");
  assert.equal(titleFromResponse(null), "");
  assert.equal(titleFromResponse({ error: { message: "x" } }), "");
  assert.equal(cleanTitle("Title: Sales report\nmore text"), "Sales report");
  assert.equal(cleanTitle("**Q3 plan**"), "Q3 plan");
  assert.equal([...cleanTitle("長".repeat(200))].length, TITLE_MAX);
});

test("the fallback title: the first few words of the first message", () => {
  assert.equal(fallbackTitle("Hello there"), "Hello there");
  assert.equal(fallbackTitle("  Please   summarise the news about the airport today, thanks "), "Please summarise the news about the…");
  assert.equal(fallbackTitle("幫我寫一封給供應商的電郵，要求延遲交貨兩星期"), "幫我寫一封給供應商的電郵，要求延…");
  assert.equal(fallbackTitle("你好"), "你好");
  assert.equal(fallbackTitle("   "), "");
  assert.equal(fallbackTitle(""), "");
  // one long token (a URL): cut by characters
  assert.equal(fallbackTitle("https://example.org/a/very/long/path/that/goes/on"), "https://example…");
  assert.ok([...fallbackTitle("x ".repeat(500))].length <= TITLE_MAX);
});

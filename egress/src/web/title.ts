// A conversation's title (docs/CHAT.md, "Titles"): one small model call through egress after the first finished turn,
// or, when that fails, the first words of the user's first message, so no conversation stays untitled.

/** The longest title kept, in characters. */
export const TITLE_MAX = 80;

const SYSTEM =
  "Write a short title (at most 6 words) for this conversation, in the language the user wrote in. Reply with the title only, no quotes.";

/**
 * The title call's body (Anthropic Messages, to egress's model route). The models think: Sonnet 5.5 on OpenRouter
 * refuses `thinking: disabled` ("Reasoning is mandatory for this endpoint", 2026-10-21), and with thinking on, the old
 * 40-token max_tokens went entirely to thinking (no text). So thinking runs at the smallest budget the API accepts
 * (1024) and max_tokens leaves room for that budget plus a short title.
 */
export function titleRequest(model: string, firstMessage: string, reply: string): Record<string, unknown> {
  return {
    model,
    max_tokens: 2048,
    thinking: { type: "enabled", budget_tokens: 1024 },
    system: SYSTEM,
    messages: [{ role: "user", content: `User: ${firstMessage.slice(0, 2000)}\n\nAssistant: ${reply.slice(0, 1000)}` }],
  };
}

/** The title in a Messages response: its final text block (thinking blocks ignored), cleaned; "" when there is none. */
export function titleFromResponse(body: unknown): string {
  const content = (body as { content?: { type?: string; text?: unknown }[] } | null)?.content;
  if (!Array.isArray(content)) return "";
  const texts = content.filter((c) => c?.type === "text" && typeof c.text === "string").map((c) => c.text as string);
  for (let i = texts.length - 1; i >= 0; i--) {
    const t = cleanTitle(texts[i]!);
    if (t) return t;
  }
  return "";
}

/** One line, no surrounding quotes, collapsed spaces, at most TITLE_MAX characters. */
export function cleanTitle(s: string): string {
  const line = s.split("\n").map((l) => l.trim()).find((l) => l) ?? "";
  const t = line
    .replace(/^(title|標題|标题)\s*[:：]\s*/i, "")
    .replace(/^["'“”‘’「『《*#\s]+|["'“”‘’」』》*\s]+$/g, "")
    .replace(/\s+/g, " ")
    .trim();
  return cut(t, TITLE_MAX);
}

/**
 * The fallback title: the first words of the user's first message (6 words; text without spaces, like Chinese,
 * its first 16 characters), "…" when cut. "" for an empty message.
 */
export function fallbackTitle(firstMessage: string): string {
  const text = firstMessage.replace(/\s+/g, " ").trim();
  if (!text) return "";
  const words = text.split(" ");
  let t: string;
  if (words.length > 1 && words.slice(0, 6).every((w) => w.length <= 20)) t = words.slice(0, 6).join(" ");
  else t = [...text].slice(0, 16).join("");
  t = cut(t, TITLE_MAX - 1);
  return t.length < text.length ? `${t.replace(/[\s,.;:!?，。；：！？、]+$/, "")}…` : t;
}

function cut(s: string, max: number): string {
  const cps = [...s];
  return cps.length > max ? cps.slice(0, max).join("").trimEnd() : s;
}

// The model route's request body rewrite (docs/CHAT.md, "egress"): zero data retention forced into `provider` and the
// thinking level applied, without materialising the body. A Claude Code request carries the whole conversation
// (images as base64 included) and reaches several MB; parsing it into objects and serialising it back costs many
// times its size, which once took egress past its memory limit. Instead one strict pass over the bytes validates the
// JSON and finds the top-level members; the output is slices of the original Buffer plus the few rewritten members,
// so a request costs about its own size in memory.
import type { ReasoningPlan } from "../common/config.ts";

/** OpenRouter provider options every model request carries (docs: guides/features/zdr "Per-Request ZDR Enforcement",
 * guides/routing/provider-selection): only Zero Data Retention endpoints, none that collect data. Set over whatever the
 * caller sent, so `zdr: false` or `data_collection: "allow"` in a request can't loosen it. */
export const FORCED_PROVIDER = { zdr: true, data_collection: "deny" } as const;

export interface ModelBody {
  /** The body's top-level "model" as JSON.parse would give it (undefined when absent). */
  model: unknown;
  /** The rewritten body as slices of the original Buffer and new members, in order. */
  parts: Buffer[];
  /** Total bytes of parts. */
  length: number;
}

interface Member {
  key: string | null; // null: a key too long to be one this module looks at
  start: number; // the key's opening quote
  valueStart: number;
  end: number; // just past the value
}

/** The body with FORCED_PROVIDER merged into `provider` and the model's thinking level from `plan` applied; null when
 * the body is not one valid JSON object (RFC 8259, UTF-8). Duplicate keys resolve as JSON.parse does (the last wins);
 * every occurrence of a rewritten key is dropped. */
export function forceZdr(raw: Buffer, plan?: ReasoningPlan): ModelBody | null {
  const members = topLevelMembers(raw);
  if (!members) return null;
  const last = (k: string) => members.findLast((m) => m.key === k);
  const value = (m: Member | undefined): unknown => (m ? JSON.parse(raw.toString("utf8", m.valueStart, m.end)) : undefined);

  const modelM = last("model");
  const model = value(modelM);
  const p = value(last("provider"));
  const provider = { ...(p && typeof p === "object" && !Array.isArray(p) ? p : {}), ...FORCED_PROVIDER };
  const drop = new Set(["provider"]);
  const add: Record<string, unknown> = { provider };

  const effort = typeof model === "string" ? planned(plan, model, hasEntries(raw, last("tools"))) : undefined;
  if (effort === "none") {
    // On OpenRouter's Messages API only Anthropic's `thinking: {type: "disabled"}` turns a reasoning model's thinking
    // off (`reasoning.effort: "none"` is ignored there).
    drop.add("thinking").add("reasoning");
    add.thinking = { type: "disabled" };
  } else if (effort) {
    drop.add("reasoning");
    add.reasoning = { effort };
  }

  const parts: Buffer[] = [Buffer.from("{")];
  let first = true;
  for (const m of members) {
    if (m.key !== null && drop.has(m.key)) continue;
    if (!first) parts.push(Buffer.from(","));
    parts.push(raw.subarray(m.start, m.end));
    first = false;
  }
  const extra = JSON.stringify(add).slice(1, -1);
  parts.push(Buffer.from((first ? "" : ",") + extra + "}"));
  return { model, parts, length: parts.reduce((n, b) => n + b.length, 0) };
}

/** The thinking level for a request: chat turns carry tools, titles and Claude Code's quick calls don't. */
function planned(plan: ReasoningPlan | undefined, model: string, withTools: boolean): string | undefined {
  if (!plan) return undefined;
  const bare = (m: string) => m.slice(m.lastIndexOf("/") + 1);
  const entry = plan[model] ?? Object.entries(plan).find(([id]) => bare(id) === model)?.[1];
  return withTools ? entry?.withTools : entry?.withoutTools;
}

/** Whether a member's value is a non-empty array. */
function hasEntries(raw: Buffer, m: Member | undefined): boolean {
  if (!m || at(raw, m.valueStart) !== 0x5b) return false;
  return at(raw, skipWs(raw, m.valueStart + 1)) !== 0x5d;
}

/** Bytes of request bodies held in memory at once. take(n) resolves once n bytes are free, first come first served;
 * a request larger than the whole budget counts as the whole budget, so it runs alone rather than never. */
export class BodyBudget {
  #free: number;
  readonly #queue: { n: number; go: () => void }[] = [];

  readonly total: number;

  constructor(total: number) {
    this.total = total;
    this.#free = total;
  }

  /** Waits for n bytes; the function it gives back returns them (calling it again does nothing). */
  async take(n: number): Promise<() => void> {
    n = Math.min(n, this.total);
    if (!this.#queue.length && n <= this.#free) this.#free -= n;
    else await new Promise<void>((go) => this.#queue.push({ n, go }));
    let returned = false;
    return () => {
      if (returned) return;
      returned = true;
      this.#free += n;
      this.#pump();
    };
  }

  /** Bytes taken right now and requests waiting (tests, the operator's logs). */
  get state(): { taken: number; waiting: number } {
    return { taken: this.total - this.#free, waiting: this.#queue.length };
  }

  #pump(): void {
    for (let w = this.#queue[0]; w && w.n <= this.#free; w = this.#queue[0]) {
      this.#queue.shift();
      this.#free -= w.n;
      w.go();
    }
  }
}

// ---- the strict scanner ----------------------------------------------------------------------------------------

/** Keys longer than this (in bytes, escapes included) are never decoded: none of the keys looked at comes close. */
const KEY_DECODE_MAX = 256;

class Invalid extends Error {}

/** The byte at i, -1 past the end. */
const at = (b: Buffer, i: number): number => b[i] ?? -1;

const isWs = (c: number) => c === 0x20 || c === 0x09 || c === 0x0a || c === 0x0d;
const isDigit = (c: number) => c >= 0x30 && c <= 0x39;
const isHex = (c: number) => isDigit(c) || (c >= 0x41 && c <= 0x46) || (c >= 0x61 && c <= 0x66);

function skipWs(b: Buffer, i: number): number {
  while (i < b.length && isWs(at(b, i))) i++;
  return i;
}

/** The members of the top-level object, or null when the body is not exactly one valid JSON object. */
export function topLevelMembers(b: Buffer): Member[] | null {
  try {
    let i = skipWs(b, 0);
    if (at(b, i) !== 0x7b) return null;
    i = skipWs(b, i + 1);
    const members: Member[] = [];
    if (at(b, i) === 0x7d) i++;
    else {
      for (;;) {
        if (at(b, i) !== 0x22) throw new Invalid();
        const start = i;
        i = scanString(b, i);
        const key = i - start <= KEY_DECODE_MAX ? (JSON.parse(b.toString("utf8", start, i)) as string) : null;
        i = skipWs(b, i);
        if (at(b, i) !== 0x3a) throw new Invalid();
        const valueStart = skipWs(b, i + 1);
        i = scanValue(b, valueStart);
        members.push({ key, start, valueStart, end: i });
        i = skipWs(b, i);
        if (at(b, i) === 0x2c) i = skipWs(b, i + 1);
        else if (at(b, i) === 0x7d) {
          i++;
          break;
        } else throw new Invalid();
      }
    }
    return skipWs(b, i) === b.length ? members : null;
  } catch (e) {
    if (e instanceof Invalid) return null;
    throw e;
  }
}

/** Past one JSON value starting at i (no leading whitespace). Iterative, so nesting depth costs no stack. */
function scanValue(b: Buffer, i: number): number {
  const stack: number[] = []; // 0x7d (in an object) or 0x5d (in an array)
  for (;;) {
    // a value at i
    const c = at(b, i);
    if (c === 0x7b || c === 0x5b) {
      const close = c === 0x7b ? 0x7d : 0x5d;
      i = skipWs(b, i + 1);
      if (at(b, i) === close) i++;
      else {
        stack.push(close);
        if (close === 0x7d) i = objectKey(b, i);
        continue;
      }
    } else if (c === 0x22) i = scanString(b, i);
    else if (c === 0x2d || isDigit(c)) i = scanNumber(b, i);
    else if (lit(b, i, "true")) i += 4;
    else if (lit(b, i, "false")) i += 5;
    else if (lit(b, i, "null")) i += 4;
    else throw new Invalid();
    // after a value: a separator, closes, or the end of this value
    for (;;) {
      if (!stack.length) return i;
      i = skipWs(b, i);
      const top = stack[stack.length - 1];
      if (at(b, i) === 0x2c) {
        i = skipWs(b, i + 1);
        if (top === 0x7d) i = objectKey(b, i);
        break;
      }
      if (at(b, i) !== top) throw new Invalid();
      stack.pop();
      i++;
    }
  }
}

/** Past `"key" :` and the whitespace after it, at i (the key's quote). */
function objectKey(b: Buffer, i: number): number {
  if (at(b, i) !== 0x22) throw new Invalid();
  i = skipWs(b, scanString(b, i));
  if (at(b, i) !== 0x3a) throw new Invalid();
  return skipWs(b, i + 1);
}

function lit(b: Buffer, i: number, s: string): boolean {
  if (i + s.length > b.length) return false;
  for (let k = 0; k < s.length; k++) if (at(b, i + k) !== s.charCodeAt(k)) return false;
  return true;
}

function scanNumber(b: Buffer, i: number): number {
  if (at(b, i) === 0x2d) i++;
  if (at(b, i) === 0x30) i++;
  else if (at(b, i) >= 0x31 && at(b, i) <= 0x39) while (isDigit(at(b, i))) i++;
  else throw new Invalid();
  if (at(b, i) === 0x2e) {
    i++;
    if (!isDigit(at(b, i))) throw new Invalid();
    while (isDigit(at(b, i))) i++;
  }
  if (at(b, i) === 0x65 || at(b, i) === 0x45) {
    i++;
    if (at(b, i) === 0x2b || at(b, i) === 0x2d) i++;
    if (!isDigit(at(b, i))) throw new Invalid();
    while (isDigit(at(b, i))) i++;
  }
  return i;
}

/** Past a string at i (its opening quote): escapes checked, no raw control characters, well-formed UTF-8 only (no
 * overlong forms, surrogates or code points past U+10FFFF), so no reader can disagree with this one about where it
 * ends. */
function scanString(b: Buffer, i: number): number {
  i++;
  for (;;) {
    if (i >= b.length) throw new Invalid();
    const c = at(b, i);
    if (c === 0x22) return i + 1;
    if (c < 0x20) throw new Invalid();
    if (c === 0x5c) {
      const e = at(b, i + 1);
      if (e === 0x75) {
        if (!(isHex(at(b, i + 2)) && isHex(at(b, i + 3)) && isHex(at(b, i + 4)) && isHex(at(b, i + 5)))) throw new Invalid();
        i += 6;
      } else if (e === 0x22 || e === 0x5c || e === 0x2f || e === 0x62 || e === 0x66 || e === 0x6e || e === 0x72 || e === 0x74) i += 2;
      else throw new Invalid();
      continue;
    }
    if (c < 0x80) {
      i++;
      continue;
    }
    i = utf8(b, i);
  }
}

const cont = (b: Buffer, i: number) => i < b.length && (at(b, i) & 0xc0) === 0x80;

/** Past one multi-byte UTF-8 sequence at i (RFC 3629 table 3-7). */
function utf8(b: Buffer, i: number): number {
  const c = at(b, i);
  if (c >= 0xc2 && c <= 0xdf) {
    if (!cont(b, i + 1)) throw new Invalid();
    return i + 2;
  }
  if (c >= 0xe0 && c <= 0xef) {
    const d = at(b, i + 1);
    const lo = c === 0xe0 ? 0xa0 : 0x80;
    const hi = c === 0xed ? 0x9f : 0xbf;
    if (!(d >= lo && d <= hi) || !cont(b, i + 2)) throw new Invalid();
    return i + 3;
  }
  if (c >= 0xf0 && c <= 0xf4) {
    const d = at(b, i + 1);
    const lo = c === 0xf0 ? 0x90 : 0x80;
    const hi = c === 0xf4 ? 0x8f : 0xbf;
    if (!(d >= lo && d <= hi) || !cont(b, i + 2) || !cont(b, i + 3)) throw new Invalid();
    return i + 4;
  }
  throw new Invalid();
}

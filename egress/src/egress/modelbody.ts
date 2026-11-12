// The model route's request body rewrite (docs/CHAT.md, "egress"): zero data retention forced into `provider` and the
// thinking level applied, as the body streams through. A Claude Code request carries the whole conversation (images as
// base64 included) and reaches tens of MB, so egress never holds it: one strict scanner validates the JSON byte by
// byte across chunks and passes every top-level member on as it arrives, except the few small ones the rewrite
// decides on (`model`, `provider`, `thinking`, `reasoning`), which are held and written last, before the closing
// brace. A request costs a few KB of memory whatever its size.
import type { ModelPlan } from "../common/config.ts";

/** OpenRouter provider options every model request carries (docs: guides/features/zdr "Per-Request ZDR Enforcement",
 * guides/routing/provider-selection): only Zero Data Retention endpoints, none that collect data. Set over whatever the
 * caller sent, so `zdr: false` or `data_collection: "deny"` in a request can't loosen it. */
export const FORCED_PROVIDER = { zdr: true, data_collection: "deny" } as const;

/** The body is not exactly one valid JSON object (RFC 8259, UTF-8), or its held members are too large. */
export class InvalidBody extends Error {}

/** The top-level members held back and written last (every occurrence is dropped where it stood; the last wins, as
 * JSON.parse resolves duplicates). */
const HELD = new Set(["model", "provider", "thinking", "reasoning"]);
/** Bytes of held members at most: they are a model id and small option objects. */
export const HELD_MAX = 1024 * 1024;
/** Keys longer than this (in bytes, escapes included) are never decoded: none of the keys looked at comes close. */
const KEY_DECODE_MAX = 256;


// scanner states
const START = 0, OBJ_FIRST = 1, KEY = 2, COLON = 3, VALUE = 4, ARR_FIRST = 5, AFTER = 6, STR = 7, ESC = 8, HEX = 9, UTF8 = 10;
const N_MINUS = 11, N_ZERO = 12, N_INT = 13, N_DOT = 14, N_FRAC = 15, N_E = 16, N_ESIGN = 17, N_EXP = 18, LIT = 19, END = 20;
// what the bytes being scanned are kept for
const CAP_NONE = 0, CAP_KEY = 1, CAP_PASS = 2, CAP_HOLD = 3;

const isWs = (c: number) => c === 0x20 || c === 0x09 || c === 0x0a || c === 0x0d;
const isDigit = (c: number) => c >= 0x30 && c <= 0x39;
const isHex = (c: number) => isDigit(c) || (c >= 0x41 && c <= 0x46) || (c >= 0x61 && c <= 0x66);
const OPEN = Buffer.from("{");
const COMMA = Buffer.from(",");

/** A model request body rewritten as it streams: push() takes the next bytes and gives back the bytes to send on
 * (views of the chunk, never copies of it), end() the closing bytes. The result is the body with FORCED_PROVIDER merged
 * into `provider`, the model's pinned providers from `plan` set as OpenRouter's `order` + `only` with no fallback (over
 * the caller's), and its thinking level applied. Both throw InvalidBody when the body is not exactly one valid JSON
 * object: strings are checked (escapes, no raw control characters, well-formed UTF-8 only: no overlong forms,
 * surrogates or code points past U+10FFFF), so no reader can disagree with this one about where a member ends. Nesting
 * depth costs one array entry per level, no stack. */
export class ModelBodyRewriter {
  readonly #plan: ModelPlan | undefined;
  #st = START;
  readonly #stack: number[] = []; // 0x7d (in an object) or 0x5d (in an array); [0] is the body's own object
  #strIsKey = false;
  #hexLeft = 0;
  #u8need = 0;
  #u8lo = 0x80;
  #u8hi = 0xbf;
  #lit = "";
  #litPos = 0;

  #cap = CAP_NONE;
  #keyParts: Buffer[] = [];
  #keyLen = 0;
  #holdKey: string | null = null; // the held member whose value is next or being read
  #holdParts: Buffer[] = [];
  #heldBytes = 0;
  readonly #held = new Map<string, Buffer>();
  #any = false; // a member has been passed on (the next one needs a comma)
  #toolsNext = false; // the value starting next is `tools`
  #toolsCheck = false; // the next byte after its `[` tells whether it has entries
  #hasTools = false;
  #model: unknown = undefined;
  #modelSeen = false;

  constructor(plan?: ModelPlan) {
    this.#plan = plan;
  }

  /** The body's "model" as JSON.parse would give it, from the last complete `model` member so far (undefined: none). */
  get model(): unknown {
    return this.#model;
  }

  /** Whether a complete `model` member has been read. */
  get modelSeen(): boolean {
    return this.#modelSeen;
  }

  push(b: Buffer): Buffer[] {
    const out: Buffer[] = [];
    const n = b.length;
    const stack = this.#stack;
    let st = this.#st;
    let i = 0;
    let from = 0; // where the bytes kept for #cap start in this chunk

    // the kept bytes b[from, end): a key is collected, a passed member goes out, a held value is copied
    const take = (end: number) => {
      if (end <= from) return;
      const part = b.subarray(from, end);
      if (this.#cap === CAP_PASS) out.push(part);
      else if (this.#cap === CAP_HOLD) {
        this.#heldBytes += part.length;
        if (this.#heldBytes > HELD_MAX) throw new InvalidBody("held members too large");
        this.#holdParts.push(Buffer.from(part));
      } else if (this.#cap === CAP_KEY) {
        this.#keyLen += part.length;
        if (this.#keyLen > KEY_DECODE_MAX) {
          // a key too long to be one of ours: pass it on from here
          this.#passKey(out);
          out.push(part);
          this.#cap = CAP_PASS;
        } else this.#keyParts.push(Buffer.from(part));
      }
      from = end;
    };
    // a top-level key ended at `end`: decide what happens to its member
    const endKey = (end: number) => {
      take(end);
      if (this.#cap === CAP_PASS) return; // too long, already going out
      const key = JSON.parse(Buffer.concat(this.#keyParts).toString("utf8")) as string;
      if (HELD.has(key)) {
        this.#holdKey = key;
        this.#keyParts = [];
        this.#cap = CAP_NONE;
      } else {
        this.#toolsNext = key === "tools";
        this.#passKey(out);
        this.#cap = CAP_PASS;
        from = end;
      }
    };
    // a value ended at `end`; when it is a top-level member's, the member is complete
    const valueDone = (end: number) => {
      if (stack.length !== 1) return;
      take(end);
      if (this.#cap === CAP_HOLD) {
        const key = this.#holdKey!;
        const raw = Buffer.concat(this.#holdParts);
        this.#held.set(key, raw);
        if (key === "model") {
          this.#model = JSON.parse(raw.toString("utf8"));
          this.#modelSeen = true;
        }
        this.#holdKey = null;
        this.#holdParts = [];
      }
      this.#cap = CAP_NONE;
    };

    while (i < n) {
      const c = b[i]!;
      switch (st) {
        case STR: {
          // the bulk of a body: plain characters of a string
          let k = i;
          while (k < n) {
            const d = b[k]!;
            if (d === 0x22 || d === 0x5c || d < 0x20 || d >= 0x80) break;
            k++;
          }
          if (k === n) {
            i = n;
            break;
          }
          const d = b[k]!;
          i = k + 1;
          if (d === 0x22) {
            if (this.#strIsKey) {
              if (stack.length === 1) endKey(i);
              st = COLON;
            } else {
              valueDone(i);
              st = AFTER;
            }
          } else if (d === 0x5c) st = ESC;
          else if (d < 0x20) throw new InvalidBody();
          else {
            if (d >= 0xc2 && d <= 0xdf) [this.#u8need, this.#u8lo, this.#u8hi] = [1, 0x80, 0xbf];
            else if (d >= 0xe0 && d <= 0xef) [this.#u8need, this.#u8lo, this.#u8hi] = [2, d === 0xe0 ? 0xa0 : 0x80, d === 0xed ? 0x9f : 0xbf];
            else if (d >= 0xf0 && d <= 0xf4) [this.#u8need, this.#u8lo, this.#u8hi] = [3, d === 0xf0 ? 0x90 : 0x80, d === 0xf4 ? 0x8f : 0xbf];
            else throw new InvalidBody();
            st = UTF8;
          }
          break;
        }
        case UTF8:
          if (c < this.#u8lo || c > this.#u8hi) throw new InvalidBody();
          this.#u8lo = 0x80;
          this.#u8hi = 0xbf;
          if (--this.#u8need === 0) st = STR;
          i++;
          break;
        case ESC:
          if (c === 0x75) {
            this.#hexLeft = 4;
            st = HEX;
          } else if (c === 0x22 || c === 0x5c || c === 0x2f || c === 0x62 || c === 0x66 || c === 0x6e || c === 0x72 || c === 0x74) st = STR;
          else throw new InvalidBody();
          i++;
          break;
        case HEX:
          if (!isHex(c)) throw new InvalidBody();
          if (--this.#hexLeft === 0) st = STR;
          i++;
          break;
        case START:
          if (isWs(c)) i++;
          else if (c === 0x7b) {
            out.push(OPEN);
            stack.push(0x7d);
            st = OBJ_FIRST;
            i++;
          } else throw new InvalidBody();
          break;
        case OBJ_FIRST:
        case KEY:
          if (isWs(c)) i++;
          else if (c === 0x22) {
            this.#strIsKey = true;
            if (stack.length === 1) {
              this.#cap = CAP_KEY;
              this.#keyParts = [];
              this.#keyLen = 0;
              from = i;
            }
            st = STR;
            i++;
          } else if (c === 0x7d && st === OBJ_FIRST) {
            stack.pop();
            i++;
            if (stack.length) {
              valueDone(i);
              st = AFTER;
            } else st = END;
          } else throw new InvalidBody();
          break;
        case COLON:
          if (isWs(c)) i++;
          else if (c === 0x3a) {
            st = VALUE;
            i++;
          } else throw new InvalidBody();
          break;
        case VALUE:
        case ARR_FIRST:
          if (isWs(c)) {
            i++;
            break;
          }
          if (st === ARR_FIRST) {
            if (this.#toolsCheck && stack.length === 2) {
              this.#hasTools = c !== 0x5d;
              this.#toolsCheck = false;
            }
            if (c === 0x5d) {
              stack.pop();
              i++;
              valueDone(i);
              st = AFTER;
              break;
            }
          }
          if (stack.length === 1) {
            // a top-level member's value starts here
            if (this.#holdKey !== null) {
              // an earlier occurrence of the member is replaced
              const old = this.#held.get(this.#holdKey);
              if (old) this.#heldBytes -= old.length;
              this.#held.delete(this.#holdKey);
              this.#cap = CAP_HOLD;
              this.#holdParts = [];
              from = i;
            }
            if (this.#toolsNext) {
              this.#toolsNext = false;
              this.#hasTools = false;
              this.#toolsCheck = c === 0x5b;
            }
          }
          if (c === 0x7b) {
            stack.push(0x7d);
            st = OBJ_FIRST;
          } else if (c === 0x5b) {
            stack.push(0x5d);
            st = ARR_FIRST;
          } else if (c === 0x22) {
            this.#strIsKey = false;
            st = STR;
          } else if (c === 0x2d) st = N_MINUS;
          else if (c === 0x30) st = N_ZERO;
          else if (c >= 0x31 && c <= 0x39) st = N_INT;
          else if (c === 0x74 || c === 0x66 || c === 0x6e) {
            this.#lit = c === 0x74 ? "true" : c === 0x66 ? "false" : "null";
            this.#litPos = 1;
            st = LIT;
          } else throw new InvalidBody();
          i++;
          break;
        case AFTER: {
          if (isWs(c)) {
            i++;
            break;
          }
          const top = stack[stack.length - 1]!;
          if (c === 0x2c) {
            st = top === 0x7d ? KEY : VALUE;
            i++;
          } else if (c === top) {
            stack.pop();
            i++;
            if (stack.length) valueDone(i);
            else st = END;
          } else throw new InvalidBody();
          break;
        }
        case LIT:
          if (c !== this.#lit.charCodeAt(this.#litPos)) throw new InvalidBody();
          i++;
          if (++this.#litPos === this.#lit.length) {
            valueDone(i);
            st = AFTER;
          }
          break;
        // numbers: one ends at the first byte that can't continue it, which AFTER then reads
        case N_MINUS:
          if (c === 0x30) st = N_ZERO;
          else if (c >= 0x31 && c <= 0x39) st = N_INT;
          else throw new InvalidBody();
          i++;
          break;
        case N_ZERO:
        case N_INT:
        case N_FRAC:
        case N_EXP:
          if (isDigit(c) && st !== N_ZERO) i++;
          else if (c === 0x2e && (st === N_ZERO || st === N_INT)) {
            st = N_DOT;
            i++;
          } else if ((c === 0x65 || c === 0x45) && st !== N_EXP) {
            st = N_E;
            i++;
          } else {
            valueDone(i);
            st = AFTER;
          }
          break;
        case N_DOT:
          if (!isDigit(c)) throw new InvalidBody();
          st = N_FRAC;
          i++;
          break;
        case N_E:
          if (c === 0x2b || c === 0x2d) st = N_ESIGN;
          else if (isDigit(c)) st = N_EXP;
          else throw new InvalidBody();
          i++;
          break;
        case N_ESIGN:
          if (!isDigit(c)) throw new InvalidBody();
          st = N_EXP;
          i++;
          break;
        case END:
          if (!isWs(c)) throw new InvalidBody();
          i++;
          break;
        default:
          throw new Error(`modelbody: state ${st}`);
      }
    }
    take(n);
    this.#st = st;
    return out;
  }

  /** The end of the body: the held members as rewritten, and the closing brace. */
  end(): Buffer {
    if (this.#st !== END) throw new InvalidBody();
    const raw = (k: string) => this.#held.get(k);
    const value = (k: string): unknown => (raw(k) ? JSON.parse(raw(k)!.toString("utf8")) : undefined);
    const model = this.#model;
    const p = value("provider");
    const pinned = typeof model === "string" ? entryOf(this.#plan, model)?.providers : undefined;
    const provider = {
      ...(p && typeof p === "object" && !Array.isArray(p) ? p : {}),
      ...(pinned ? { order: pinned, only: pinned, allow_fallbacks: false } : {}),
      ...FORCED_PROVIDER,
    };
    const keep = new Set(["model", "thinking", "reasoning"]);
    const add: Record<string, unknown> = { provider };
    const effort = typeof model === "string" ? planned(this.#plan, model, this.#hasTools) : undefined;
    if (effort === "none") {
      // On OpenRouter's Messages API only Anthropic's `thinking: {type: "disabled"}` turns a reasoning model's thinking
      // off (`reasoning.effort: "none"` is ignored there).
      keep.delete("thinking");
      keep.delete("reasoning");
      add.thinking = { type: "disabled" };
    } else if (effort) {
      keep.delete("reasoning");
      add.reasoning = { effort };
    }
    const parts: Buffer[] = [];
    for (const k of keep) {
      const r = raw(k);
      if (!r) continue;
      parts.push(Buffer.from(`${this.#any || parts.length ? "," : ""}${JSON.stringify(k)}:`), r);
    }
    parts.push(Buffer.from(`${this.#any || parts.length ? "," : ""}${JSON.stringify(add).slice(1, -1)}}`));
    return Buffer.concat(parts);
  }

  /** The collected key goes out, after a comma when a member went before it. */
  #passKey(out: Buffer[]): void {
    if (this.#any) out.push(COMMA);
    this.#any = true;
    out.push(...this.#keyParts);
    this.#keyParts = [];
  }
}

/** The thinking level for a request: chat turns carry tools, titles and Claude Code's quick calls don't. */
function planned(plan: ModelPlan | undefined, model: string, withTools: boolean): string | undefined {
  const entry = entryOf(plan, model);
  return withTools ? entry?.withTools : entry?.withoutTools;
}

/** A model's plan entry, by its full id or, as clients may send it, without the provider prefix. */
function entryOf(plan: ModelPlan | undefined, model: string): ModelPlan[string] | undefined {
  if (!plan) return undefined;
  const bare = (m: string) => m.slice(m.lastIndexOf("/") + 1);
  return plan[model] ?? Object.entries(plan).find(([id]) => bare(id) === model)?.[1];
}

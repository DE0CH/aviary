// The sandbox internet rules (docs/CHAT.md, "The sandbox internet rules"): which hosts a sandbox may reach, decided
// automatically with no per-conversation or staff override. Every host passes except those the deny list names
// (Deyao, 2026-11-10: fewer false refusals). Entries are `host` (that name only) or `.domain` (the domain and every
// name under it); a `*` in an entry's first label stands for any characters inside that label (`.oss-*.aliyuncs.com`).
// A denied host still passes when an allow entry is more specific than its most specific deny entry
// (so the exact raw.githubusercontent.com passes under the denied `.githubusercontent.com`, and an equally specific
// deny wins); config `egress.denyHosts` wins over everything.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { normHost } from "./netguard.ts";

/** The generated lists shipped with the image (chat/egress-allowlist/, built by generate.py). */
export const ALLOWLIST_DIR = fileURLToPath(new URL("../../egress-allowlist/", import.meta.url));

/** A pattern entry's first label (`oss-*`), kept under the rest of its name (`aliyuncs.com`). */
type LabelPattern = { label: RegExp; suffix: boolean };

export class HostRules {
  readonly #exact = new Set<string>();
  readonly #suffix = new Set<string>();
  readonly #patterns = new Map<string, LabelPattern[]>();
  #patternCount = 0;
  constructor(entries: Iterable<string> = []) {
    this.add(entries);
  }
  add(entries: Iterable<string>): this {
    for (const raw of entries) {
      const e = normHost(raw.replace(/^\*\./, "."));
      if (!e || e === ".") continue;
      if (e.includes("*")) this.#addPattern(e);
      else if (e.startsWith(".")) this.#suffix.add(e.slice(1));
      else this.#exact.add(e);
    }
    return this;
  }
  /**
   * `.oss-*.aliyuncs.com`: a `*` in the entry's first label stands for any run of characters inside that label (a
   * provider's per-region endpoints, whose list grows). Anywhere else it is a mistake, refused rather than left as an
   * entry that matches nothing.
   */
  #addPattern(e: string): void {
    const name = e.replace(/^\./, "");
    const dot = name.indexOf(".");
    if (dot <= 0 || name.indexOf("*", dot) >= 0) throw new Error(`host entry ${JSON.stringify(e)}: a * may only be in the first label of a name with a domain after it`);
    const label = new RegExp("^" + name.slice(0, dot).split("*").map((s) => s.replace(/[.+?^${}()|[\]\\]/g, "\\$&")).join("[^.]*") + "$");
    const rest = name.slice(dot + 1);
    const list = this.#patterns.get(rest) ?? [];
    list.push({ label, suffix: e.startsWith(".") });
    this.#patterns.set(rest, list);
    this.#patternCount++;
  }
  get size(): number {
    return this.#exact.size + this.#suffix.size + this.#patternCount;
  }
  /** How specific the best entry matching `host` is: 0 = none; an exact entry beats a suffix entry of the same name. */
  match(host: string): number {
    const h = normHost(host);
    if (!h) return 0;
    if (this.#exact.has(h)) return h.length + 1;
    // h itself, then each parent: a.b.c, b.c, c
    for (let i = 0; ; ) {
      const s = h.slice(i);
      if (this.#suffix.has(s)) return s.length;
      const dot = h.indexOf(".", i);
      if (dot < 0) return 0;
      const patterns = this.#patterns.get(h.slice(dot + 1));
      if (patterns) {
        const label = h.slice(i, dot);
        // a pattern without the leading dot is that name only, like an exact entry
        if (i === 0 && patterns.some((p) => !p.suffix && p.label.test(label))) return h.length + 1;
        if (patterns.some((p) => p.suffix && p.label.test(label))) return s.length;
      }
      i = dot + 1;
    }
  }
}

/** Lines of a list file: comments (#) and blanks dropped. */
export function parseList(text: string): string[] {
  return text
    .split("\n")
    .map((l) => l.replace(/#.*/, "").trim())
    .filter(Boolean);
}

/** `# version: …` of a generated list file. */
function versionOf(text: string): string {
  return /^# version: (.+)$/m.exec(text)?.[1]?.trim() ?? "unknown";
}

export type HostVerdict =
  | { allow: true }
  | { allow: false; reason: "deny:denylist" | "deny:config" };

export class HostPolicy {
  readonly allow: HostRules;
  readonly deny: HostRules;
  readonly configDeny: HostRules;
  readonly version: string;
  constructor(o: { allow: Iterable<string>; deny?: Iterable<string>; configDeny?: Iterable<string>; version?: string }) {
    this.allow = new HostRules(o.allow);
    this.deny = new HostRules(o.deny ?? []);
    this.configDeny = new HostRules(o.configDeny ?? []);
    this.version = o.version ?? "config only";
  }

  /**
   * The instance's policy: the generated lists in `dir` (unless `generated` is false) plus config
   * egress.defaultAllowedHosts, with config egress.denyHosts on top.
   */
  static load(o: { defaultAllowedHosts: string[]; denyHosts: string[]; generated: boolean; dir?: string }): HostPolicy {
    if (!o.generated) return new HostPolicy({ allow: o.defaultAllowedHosts, configDeny: o.denyHosts });
    const dir = o.dir ?? ALLOWLIST_DIR;
    const allowText = readFileSync(`${dir}/allow.txt`, "utf8");
    const denyText = readFileSync(`${dir}/deny.txt`, "utf8");
    return new HostPolicy({
      allow: [...parseList(allowText), ...o.defaultAllowedHosts],
      deny: parseList(denyText),
      configDeny: o.denyHosts,
      version: versionOf(allowText),
    });
  }

  check(host: string): HostVerdict {
    if (this.configDeny.match(host) > 0) return { allow: false, reason: "deny:config" };
    // every host passes unless a deny entry at least as specific as its best allow entry names it (Deyao, 2026-11-10)
    const d = this.deny.match(host);
    if (d === 0 || this.allow.match(host) > d) return { allow: true };
    return { allow: false, reason: "deny:denylist" };
  }
}

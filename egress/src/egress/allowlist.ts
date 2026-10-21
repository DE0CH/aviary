// The sandbox allowlist (docs/CHAT.md, "The sandbox allowlist"): which hosts a sandbox may reach, decided
// automatically with no per-conversation or staff override. Entries are `host` (that name only) or `.domain` (the
// domain and every name under it). A host passes when its most specific allow entry is more specific than its most
// specific deny entry (so `.google.com` allows www.google.com while `.drive.google.com` denies Drive, and an equally
// specific deny wins); config `egress.denyHosts` wins over everything.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { normHost } from "./netguard.ts";

/** The generated lists shipped with the image (chat/egress-allowlist/, built by generate.py). */
export const ALLOWLIST_DIR = fileURLToPath(new URL("../../egress-allowlist/", import.meta.url));

export class HostRules {
  readonly #exact = new Set<string>();
  readonly #suffix = new Set<string>();
  constructor(entries: Iterable<string> = []) {
    this.add(entries);
  }
  add(entries: Iterable<string>): this {
    for (const raw of entries) {
      const e = normHost(raw.replace(/^\*\./, "."));
      if (!e || e === ".") continue;
      if (e.startsWith(".")) this.#suffix.add(e.slice(1));
      else this.#exact.add(e);
    }
    return this;
  }
  get size(): number {
    return this.#exact.size + this.#suffix.size;
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
  | { allow: false; reason: "deny:host" | "deny:denylist" | "deny:config" };

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
    const a = this.allow.match(host);
    const d = this.deny.match(host);
    if (a > d) return { allow: true };
    return { allow: false, reason: d > 0 ? "deny:denylist" : "deny:host" };
  }
}

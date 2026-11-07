// What egress and tools share of their configuration (each reads its own file: egress /etc/egress/egress.json,
// tools /etc/tools/tools.json; docs/CHAT.md). Secrets come from environment variables, and a string in a config may
// name one as ${VAR} (expanded where it is used, never at load: expandEnv()).

/** OpenRouter's reasoning levels; "none" turns thinking off. */
export type ReasoningEffort = "none" | "low" | "medium" | "high" | "max";

/** How egress sets thinking per model: `withTools` for chat turns, `withoutTools` for tool-less calls. */
export type ReasoningPlan = Record<string, { withTools?: ReasoningEffort; withoutTools?: ReasoningEffort }>;

/** Expand ${VAR} from `vars` (default process.env); an unset variable is an error, never an empty string. */
export function expandEnv(s: string, vars: Record<string, string | undefined> = process.env): string {
  return s.replace(/\$\{([A-Z0-9_]+)\}/g, (_, name: string) => {
    const v = vars[name];
    if (v === undefined || v === "") throw new Error(`environment variable ${name} is not set`);
    return v;
  });
}

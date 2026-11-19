import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/go";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  go: grammar,
};

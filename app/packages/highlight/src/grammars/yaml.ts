import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/yaml";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  yaml: grammar,
  yml: grammar,
};

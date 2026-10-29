import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/json";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  json: grammar,
};

import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/java";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  java: grammar,
};

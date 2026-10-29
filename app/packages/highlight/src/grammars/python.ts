import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/python";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  py: grammar,
};

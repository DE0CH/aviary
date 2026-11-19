import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/rust";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  rs: grammar,
};

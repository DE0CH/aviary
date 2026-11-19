import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/xml";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  xml: grammar,
};

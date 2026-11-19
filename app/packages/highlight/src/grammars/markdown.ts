import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/markdown";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  md: grammar,
  mdx: grammar,
};

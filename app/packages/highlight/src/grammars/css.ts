import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/css";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  css: grammar,
  scss: grammar,
};

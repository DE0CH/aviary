import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/html";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  html: grammar,
  htm: grammar,
};

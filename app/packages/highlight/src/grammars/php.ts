import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/php";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  php: grammar,
};

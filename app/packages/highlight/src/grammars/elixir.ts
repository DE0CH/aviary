import type { Grammar } from "../grammar.js";
import { parser } from "lezer-elixir";

const grammar: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  ex: grammar,
  exs: grammar,
};

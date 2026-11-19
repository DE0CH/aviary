import type { Grammar } from "../grammar.js";
import { parser } from "../svelte/parser.js";
import { configureNesting, defaultNesting } from "../svelte/nesting.js";

export const grammars: Record<string, Grammar> = {
  svelte: { parser: parser.configure({ wrap: configureNesting(defaultNesting) }) },
};

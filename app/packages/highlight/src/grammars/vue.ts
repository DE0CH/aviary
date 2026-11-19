import type { Grammar } from "../grammar.js";
import { vueParser } from "../vue/parser.js";

export const grammars: Record<string, Grammar> = { vue: { parser: vueParser } };

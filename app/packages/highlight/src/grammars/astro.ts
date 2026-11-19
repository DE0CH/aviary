import type { Grammar } from "../grammar.js";
import { astroParser } from "../astro/parser.js";

export const grammars: Record<string, Grammar> = { astro: { parser: astroParser } };

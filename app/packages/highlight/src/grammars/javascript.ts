import type { Grammar } from "../grammar.js";
import { parser } from "@lezer/javascript";

const js: Grammar = { parser };

export const grammars: Record<string, Grammar> = {
  js,
  mjs: js,
  cjs: js,
  jsx: { parser: parser.configure({ dialect: "jsx" }) },
  ts: { parser: parser.configure({ dialect: "ts" }) },
  tsx: { parser: parser.configure({ dialect: "ts jsx" }) },
};

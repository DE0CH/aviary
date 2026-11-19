import type { Grammar } from "../grammar.js";
import { StreamLanguage, type Language } from "@codemirror/language";
import { dart } from "@codemirror/legacy-modes/mode/clike";
import { swift } from "@codemirror/legacy-modes/mode/swift";
import { csharpLanguage } from "../csharp/language.js";
import { nixLanguage } from "../nix/language.js";

// The grammars that are CodeMirror languages themselves (stream modes and LR languages with their editor props):
// one chunk, so @codemirror/language comes once for all four.
function fromLanguage(language: Language): Grammar {
  return { parser: language.parser, language };
}

export const grammars: Record<string, Grammar> = {
  swift: fromLanguage(StreamLanguage.define(swift)),
  dart: fromLanguage(StreamLanguage.define(dart)),
  cs: fromLanguage(csharpLanguage),
  nix: fromLanguage(nixLanguage),
};

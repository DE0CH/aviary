import type { Parser } from "@lezer/common";
import type { Language } from "@codemirror/language";

/**
 * One language's grammar: its Lezer parser, and for the languages defined as CodeMirror languages (stream modes, LR
 * languages with editor props) that language, which the editor uses as it is (codemirror.ts wraps the others).
 */
export interface Grammar {
  parser: Parser;
  language?: Language;
}

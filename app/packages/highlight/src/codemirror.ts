import {
  defineLanguageFacet,
  HighlightStyle as CodeMirrorHighlightStyle,
  Language,
} from "@codemirror/language";
import type { Grammar } from "./grammar.js";
import { syntaxRoleTags } from "./syntax-roles.js";
import type { HighlightStyle } from "./types.js";

// The CodeMirror side (the source view and editor): kept out of the package's main entry, so highlighting code
// blocks never loads CodeMirror.

const languages = new WeakMap<Grammar, Language>();

/** The CodeMirror language for a loaded grammar (parsers.ts loadGrammarForFile). */
export function codeMirrorLanguage(grammar: Grammar): Language {
  if (grammar.language) return grammar.language;
  let language = languages.get(grammar);
  if (!language) {
    language = new Language(defineLanguageFacet(), grammar.parser);
    languages.set(grammar, language);
  }
  return language;
}

export function createCodeMirrorHighlightStyle(colors: Record<HighlightStyle, string>) {
  return CodeMirrorHighlightStyle.define(
    syntaxRoleTags.map(({ tag, role }) => ({ tag, color: colors[role] })),
  );
}

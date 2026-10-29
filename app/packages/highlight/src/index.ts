export type { HighlightStyle, HighlightToken } from "./types.js";
export type { Grammar } from "./grammar.js";
export {
  getGrammarForFile,
  getParserForFile,
  isGrammarLoaded,
  isLanguageSupported,
  loadGrammarForFile,
  getSupportedExtensions,
} from "./parsers.js";
export { highlightCode, highlightCodeInSteps, highlightLine } from "./highlighter.js";
export { darkHighlightColors, lightHighlightColors } from "./colors.js";
export type { SyntaxThemeId, SyntaxThemeOption, SyntaxColors } from "./themes.js";
export {
  SYNTAX_THEME_IDS,
  SYNTAX_THEME_OPTIONS,
  isSyntaxThemeId,
  resolveSyntaxColors,
} from "./themes.js";

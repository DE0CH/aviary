import type { Parser } from "@lezer/common";
import type { Grammar } from "./grammar.js";

// Each language family's grammar is its own module, imported the first time a file in that language is highlighted
// (a bundler splits each import() into its own chunk: the app fetches a grammar when a code block in that language
// first renders, docs/paseo-ui.md "Loading"). The table below is all that loads up front.
type GrammarFamily = () => Promise<{ grammars: Record<string, Grammar> }>;

const javascript: GrammarFamily = () => import("./grammars/javascript.js");
const cpp: GrammarFamily = () => import("./grammars/cpp.js");
const json: GrammarFamily = () => import("./grammars/json.js");
const css: GrammarFamily = () => import("./grammars/css.js");
const html: GrammarFamily = () => import("./grammars/html.js");
const svelte: GrammarFamily = () => import("./grammars/svelte.js");
const astro: GrammarFamily = () => import("./grammars/astro.js");
const vue: GrammarFamily = () => import("./grammars/vue.js");
const xml: GrammarFamily = () => import("./grammars/xml.js");
const java: GrammarFamily = () => import("./grammars/java.js");
const python: GrammarFamily = () => import("./grammars/python.js");
const go: GrammarFamily = () => import("./grammars/go.js");
const php: GrammarFamily = () => import("./grammars/php.js");
const yaml: GrammarFamily = () => import("./grammars/yaml.js");
const rust: GrammarFamily = () => import("./grammars/rust.js");
const editorModes: GrammarFamily = () => import("./grammars/editor-modes.js");
const elixir: GrammarFamily = () => import("./grammars/elixir.js");
const markdown: GrammarFamily = () => import("./grammars/markdown.js");

const familiesByExtension: Record<string, GrammarFamily> = {
  // JavaScript/TypeScript
  js: javascript,
  jsx: javascript,
  ts: javascript,
  tsx: javascript,
  mjs: javascript,
  cjs: javascript,
  // C / C++ / Objective-C
  c: cpp,
  h: cpp,
  cc: cpp,
  cpp: cpp,
  cxx: cpp,
  hpp: cpp,
  hxx: cpp,
  m: cpp,
  mm: cpp,
  json,
  css,
  scss: css,
  html,
  htm: html,
  svelte,
  astro,
  vue,
  xml,
  java,
  py: python,
  go,
  php,
  yaml,
  yml: yaml,
  rs: rust,
  // Swift, Dart, C#, Nix: CodeMirror languages
  swift: editorModes,
  dart: editorModes,
  cs: editorModes,
  nix: editorModes,
  ex: elixir,
  exs: elixir,
  md: markdown,
  mdx: markdown,
};

const loadedGrammars = new Map<string, Grammar>();
const loadingFamilies = new Map<GrammarFamily, Promise<void>>();

function extensionOf(filename: string): string | null {
  const ext = filename.split(".").pop()?.toLowerCase();
  return ext && Object.prototype.hasOwnProperty.call(familiesByExtension, ext) ? ext : null;
}

/** The grammar for `filename`, once loadGrammarForFile has loaded it; null until then and for unknown languages. */
export function getGrammarForFile(filename: string): Grammar | null {
  const ext = extensionOf(filename);
  return ext ? (loadedGrammars.get(ext) ?? null) : null;
}

export function getParserForFile(filename: string): Parser | null {
  return getGrammarForFile(filename)?.parser ?? null;
}

export function isGrammarLoaded(filename: string): boolean {
  return getGrammarForFile(filename) !== null;
}

/**
 * Loads the grammar for `filename` (its family's module, once; a failed load is forgotten so the next call tries
 * again). Resolves to null for a language without a grammar.
 */
export function loadGrammarForFile(filename: string): Promise<Grammar | null> {
  const ext = extensionOf(filename);
  if (!ext) return Promise.resolve(null);
  const loaded = loadedGrammars.get(ext);
  if (loaded) return Promise.resolve(loaded);
  const family = familiesByExtension[ext];
  let loading = loadingFamilies.get(family);
  if (!loading) {
    loading = family().then(
      (module) => {
        for (const [key, grammar] of Object.entries(module.grammars)) {
          loadedGrammars.set(key, grammar);
        }
      },
      (error: unknown) => {
        loadingFamilies.delete(family);
        throw error;
      },
    );
    loadingFamilies.set(family, loading);
  }
  return loading.then(() => loadedGrammars.get(ext) ?? null);
}

/** Whether `filename`'s language has a grammar (loaded or not). */
export function isLanguageSupported(filename: string): boolean {
  return extensionOf(filename) !== null;
}

export function getSupportedExtensions(): string[] {
  return Object.keys(familiesByExtension);
}

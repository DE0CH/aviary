import type { Grammar } from "./grammar.js";

// The grammar loader (parsers.ts): its own chunk, holding the Lezer runtime every grammar uses, so a grammar's chunk
// carries only the grammar (the app's bundler drops modules the importing chunk already defines; docs/paseo-ui.md
// "Loading"). The re-exports keep the runtime in this module's chunk.
export { NodeProp, Tree } from "@lezer/common";
export { LRParser } from "@lezer/lr";
export { styleTags } from "@lezer/highlight";

type GrammarModule = Promise<{ grammars: Record<string, Grammar> }>;

export const families = {
  javascript: (): GrammarModule => import("./grammars/javascript.js"),
  cpp: (): GrammarModule => import("./grammars/cpp.js"),
  json: (): GrammarModule => import("./grammars/json.js"),
  css: (): GrammarModule => import("./grammars/css.js"),
  html: (): GrammarModule => import("./grammars/html.js"),
  svelte: (): GrammarModule => import("./grammars/svelte.js"),
  astro: (): GrammarModule => import("./grammars/astro.js"),
  vue: (): GrammarModule => import("./grammars/vue.js"),
  xml: (): GrammarModule => import("./grammars/xml.js"),
  java: (): GrammarModule => import("./grammars/java.js"),
  python: (): GrammarModule => import("./grammars/python.js"),
  go: (): GrammarModule => import("./grammars/go.js"),
  php: (): GrammarModule => import("./grammars/php.js"),
  yaml: (): GrammarModule => import("./grammars/yaml.js"),
  rust: (): GrammarModule => import("./grammars/rust.js"),
  editorModes: (): GrammarModule => import("./grammars/editor-modes.js"),
  elixir: (): GrammarModule => import("./grammars/elixir.js"),
  markdown: (): GrammarModule => import("./grammars/markdown.js"),
};

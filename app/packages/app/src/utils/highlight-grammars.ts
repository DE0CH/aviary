import { useEffect, useSyncExternalStore } from "react";
import type * as HighlightModule from "@getpaseo/highlight";

// The highlighter and its grammars load when code first needs them (docs/paseo-ui.md "Loading"): the highlighter is
// one chunk, fetched the first time any code renders (a reply's code block, a tool call's file, the settings preview,
// the source view), and each language's grammar is its own chunk, fetched when code in that language first renders.
// Until then code shows as plain text in the same font and lines, so nothing moves when the colours arrive.
// Every value import of @getpaseo/highlight goes through here: a static one would copy it into that chunk.

type Highlighter = typeof HighlightModule;

let highlighter: Highlighter | null = null;
let loadingHighlighter: Promise<Highlighter> | null = null;
const requested = new Set<string>();
const listeners = new Set<() => void>();

/** The highlighter (fetched once; a failed fetch is forgotten, so the next call fetches again). */
export function loadHighlighter(): Promise<Highlighter> {
  loadingHighlighter ??= import("@getpaseo/highlight").then(
    (module) => {
      highlighter = module;
      return module;
    },
    (error: unknown) => {
      loadingHighlighter = null;
      throw error;
    },
  );
  return loadingHighlighter;
}

/** The highlighter once it has arrived, else null. */
export function loadedHighlighter(): Highlighter | null {
  return highlighter;
}

function fileFor(ext: string): string {
  return `x.${ext}`;
}

/** Whether code with this extension can be highlighted now: the highlighter and the language's grammar are here. */
export function isGrammarReady(ext: string | null): boolean {
  return !!ext && !!highlighter && highlighter.isGrammarLoaded(fileFor(ext));
}

/**
 * Fetches the highlighter and the grammar for `ext` (once per extension; a failed fetch is forgotten, so the next
 * render asks again) and tells useGrammarReady's components when it lands.
 */
export function requestGrammar(ext: string | null): void {
  if (!ext || requested.has(ext) || isGrammarReady(ext)) return;
  if (highlighter && !highlighter.isLanguageSupported(fileFor(ext))) return;
  requested.add(ext);
  void loadHighlighter()
    .then((module) => module.loadGrammarForFile(fileFor(ext)))
    .then(
      (grammar) => {
        if (!grammar) return;
        for (const listener of listeners) listener();
      },
      (error: unknown) => {
        requested.delete(ext);
        console.warn(`[highlight] the ${ext} grammar did not load`, error);
      },
    );
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/**
 * Whether code with this extension can be highlighted yet; asks for its grammar when not, and re-renders the caller
 * when it arrives. Put the result in the dependencies of whatever tokenizes the code.
 */
export function useGrammarReady(ext: string | null): boolean {
  const ready = useSyncExternalStore(
    subscribe,
    () => isGrammarReady(ext),
    () => false,
  );
  useEffect(() => {
    if (!ready) requestGrammar(ext);
  }, [ext, ready]);
  return ready;
}

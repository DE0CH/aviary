import { loadHighlighter } from "./highlight-grammars";

/** Tests that highlight: the highlighter and every grammar, loaded as the app loads them on first use. */
export async function loadAllGrammarsForTests(): Promise<void> {
  const highlighter = await loadHighlighter();
  await Promise.all(
    highlighter.getSupportedExtensions().map((ext) => highlighter.loadGrammarForFile(`x.${ext}`)),
  );
}

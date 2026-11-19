// Stands in for react-native-enriched-markdown's web KaTeX loader (metro.config.cjs): the package requires KaTeX
// synchronously, which puts it in the main chunk; here it is its own chunk, fetched the first time math renders
// (docs/paseo-ui.md "Loading"). The app's renderer turns math off, so in practice it is never fetched.
interface KaTeXInstance {
  renderToString: (...args: unknown[]) => string;
}

let katexLoadPromise: Promise<KaTeXInstance | null> | null = null;

/** Lazily loads KaTeX. Resolves to null if it cannot be loaded. */
export function loadKaTeX(): Promise<KaTeXInstance | null> {
  katexLoadPromise ??= import("katex").then(
    (mod: { default?: unknown }) => {
      const candidate = (mod?.default ?? mod) as Partial<KaTeXInstance> | undefined;
      return typeof candidate?.renderToString === "function" ? (candidate as KaTeXInstance) : null;
    },
    () => null,
  );
  return katexLoadPromise;
}

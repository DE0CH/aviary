import { lazy, type ComponentType, type LazyExoticComponent } from "react";

// A chunk fetch can fail on a flaky connection, or after a deploy replaced the build the page came from. Three tries
// with backoff, then one reload of the page (once per tab, so a chunk that is really gone cannot loop); after that the
// error reaches the area's ChunkErrorBoundary (docs/paseo-ui.md "Loading").
const RETRY_DELAYS_MS = [500, 1_500];
const RELOAD_KEY = "yyc-chunk-reload";

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function reloadOnce(): boolean {
  if (typeof window === "undefined" || typeof window.location?.reload !== "function") return false;
  try {
    if (window.sessionStorage.getItem(RELOAD_KEY)) return false;
    window.sessionStorage.setItem(RELOAD_KEY, String(Date.now()));
  } catch {
    return false;
  }
  window.location.reload();
  return true;
}

/** Runs `load` up to three times; on the last failure reloads the page once, else rethrows. */
export async function loadWithRetry<T>(
  load: () => Promise<T>,
  options: { delaysMs?: readonly number[]; reload?: () => boolean } = {},
): Promise<T> {
  const delays = options.delaysMs ?? RETRY_DELAYS_MS;
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await load();
    } catch (error) {
      if (attempt < delays.length) {
        await wait(delays[attempt]);
        continue;
      }
      if ((options.reload ?? reloadOnce)()) return new Promise<T>(() => undefined);
      throw error;
    }
  }
}

/** React.lazy over a chunk, with loadWithRetry's retries. `pick` names the export to render. */
export function lazyWithRetry<Module, Props extends object>(
  load: () => Promise<Module>,
  pick: (module: Module) => ComponentType<Props>,
): LazyExoticComponent<ComponentType<Props>> {
  return lazy(async () => ({ default: pick(await loadWithRetry(load)) }));
}

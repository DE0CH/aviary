import { lazy, type ComponentType, type LazyExoticComponent } from "react";

// A chunk fetch can fail on a flaky connection, or after a deploy replaced the build the page came from. A failed
// fetch is tried three times with backoff, then the page reloads, at most once in ten minutes for the whole tab (the
// mark is never cleared by a later success, so a chunk that keeps failing beside chunks that load cannot loop); after
// that the error reaches the area's ChunkErrorBoundary (docs/paseo-ui.md "Loading"). An error the chunk throws while it
// evaluates is a bug, not a fetch: no retry and no reload, it goes straight to the boundary.
const RETRY_DELAYS_MS = [500, 1_500];
const RELOAD_KEY = "yyc-chunk-reload";
const RELOAD_AGAIN_AFTER_MS = 10 * 60_000;

/** A chunk that could not be fetched (Expo's AsyncRequireError on web), as opposed to one that threw. */
export function isChunkLoadError(error: unknown): boolean {
  if (!(error instanceof Error)) return false;
  if (error.name === "AsyncRequireError" || error.name === "ChunkLoadError") return true;
  return /^Loading (module|chunk) .* failed|Failed to fetch dynamically imported module/.test(
    error.message,
  );
}

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function reloadOnce(): boolean {
  if (typeof window === "undefined" || typeof window.location?.reload !== "function") return false;
  try {
    const last = Number(window.sessionStorage.getItem(RELOAD_KEY));
    if (last && Date.now() - last < RELOAD_AGAIN_AFTER_MS) return false;
    window.sessionStorage.setItem(RELOAD_KEY, String(Date.now()));
  } catch {
    return false;
  }
  window.location.reload();
  return true;
}

/**
 * Runs `load`; a failed fetch is tried up to three times, then the page reloads (at most once in ten minutes), else
 * the error is rethrown. Any other error is rethrown at once.
 */
export async function loadWithRetry<T>(
  load: () => Promise<T>,
  options: { delaysMs?: readonly number[]; reload?: () => boolean } = {},
): Promise<T> {
  const delays = options.delaysMs ?? RETRY_DELAYS_MS;
  for (let attempt = 0; ; attempt += 1) {
    try {
      return await load();
    } catch (error) {
      if (!isChunkLoadError(error)) throw error;
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

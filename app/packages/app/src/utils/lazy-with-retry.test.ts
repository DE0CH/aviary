import { describe, expect, it, vi } from "vitest";
import { isChunkLoadError, loadWithRetry } from "./lazy-with-retry";

function fetchFailed(): Error {
  const error = new Error(
    "Loading module https://ai.example/_expo/static/js/web/hosts.js failed.\n(error: x)",
  );
  error.name = "AsyncRequireError";
  return error;
}

describe("loadWithRetry", () => {
  it("returns the first successful load", async () => {
    const load = vi.fn().mockResolvedValue("chunk");
    await expect(loadWithRetry(load, { delaysMs: [0, 0] })).resolves.toBe("chunk");
    expect(load).toHaveBeenCalledTimes(1);
  });

  it("tries a failed fetch three times before giving up", async () => {
    const load = vi
      .fn()
      .mockRejectedValueOnce(fetchFailed())
      .mockRejectedValueOnce(fetchFailed())
      .mockResolvedValue("chunk");
    await expect(loadWithRetry(load, { delaysMs: [0, 0] })).resolves.toBe("chunk");
    expect(load).toHaveBeenCalledTimes(3);
  });

  it("reloads the page once after the third failed fetch", async () => {
    const load = vi.fn().mockRejectedValue(fetchFailed());
    const reload = vi.fn().mockReturnValue(true);
    const result = loadWithRetry(load, { delaysMs: [0, 0], reload });
    await vi.waitFor(() => expect(reload).toHaveBeenCalledTimes(1));
    expect(load).toHaveBeenCalledTimes(3);
    const settled = await Promise.race([result.then(() => "settled"), Promise.resolve("pending")]);
    expect(settled).toBe("pending");
  });

  it("rethrows an evaluation error at once, with no retry and no reload", async () => {
    const load = vi.fn().mockRejectedValue(new TypeError("x is not a function"));
    const reload = vi.fn().mockReturnValue(true);
    await expect(loadWithRetry(load, { delaysMs: [0, 0], reload })).rejects.toThrow(
      "x is not a function",
    );
    expect(load).toHaveBeenCalledTimes(1);
    expect(reload).not.toHaveBeenCalled();
  });

  it("does not reload again while another chunk loads fine (no reload loop)", async () => {
    const storage = new Map<string, string>();
    const reload = vi.fn();
    vi.stubGlobal("window", {
      location: { reload },
      sessionStorage: {
        getItem: (key: string) => storage.get(key) ?? null,
        setItem: (key: string, value: string) => storage.set(key, value),
        removeItem: (key: string) => storage.delete(key),
      },
    });
    try {
      const failing = vi.fn().mockRejectedValue(fetchFailed());
      void loadWithRetry(failing, { delaysMs: [0, 0] });
      await vi.waitFor(() => expect(reload).toHaveBeenCalledTimes(1));
      // After the reload: another chunk loads, then the same chunk fails again
      await loadWithRetry(vi.fn().mockResolvedValue("reply"), { delaysMs: [0, 0] });
      await expect(loadWithRetry(failing, { delaysMs: [0, 0] })).rejects.toThrow("Loading module");
      expect(reload).toHaveBeenCalledTimes(1);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("rethrows when the page was already reloaded", async () => {
    const load = vi.fn().mockRejectedValue(fetchFailed());
    await expect(loadWithRetry(load, { delaysMs: [0, 0], reload: () => false })).rejects.toThrow(
      "Loading module",
    );
  });
});

describe("isChunkLoadError", () => {
  it("tells a failed fetch from a thrown bug", () => {
    expect(isChunkLoadError(fetchFailed())).toBe(true);
    expect(isChunkLoadError(new Error("Loading module /a.js failed.\n(missing: /a.js)"))).toBe(true);
    expect(isChunkLoadError(new TypeError("Cannot read properties of undefined"))).toBe(false);
    expect(isChunkLoadError("offline")).toBe(false);
  });
});

import { describe, expect, it, vi } from "vitest";
import { loadWithRetry } from "./lazy-with-retry";

describe("loadWithRetry", () => {
  it("returns the first successful load", async () => {
    const load = vi.fn().mockResolvedValue("chunk");
    await expect(loadWithRetry(load, { delaysMs: [0, 0] })).resolves.toBe("chunk");
    expect(load).toHaveBeenCalledTimes(1);
  });

  it("tries three times before giving up", async () => {
    const load = vi
      .fn()
      .mockRejectedValueOnce(new Error("offline"))
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue("chunk");
    await expect(loadWithRetry(load, { delaysMs: [0, 0] })).resolves.toBe("chunk");
    expect(load).toHaveBeenCalledTimes(3);
  });

  it("reloads the page once after the third failure", async () => {
    const load = vi.fn().mockRejectedValue(new Error("gone"));
    const reload = vi.fn().mockReturnValue(true);
    const result = loadWithRetry(load, { delaysMs: [0, 0], reload });
    await vi.waitFor(() => expect(reload).toHaveBeenCalledTimes(1));
    expect(load).toHaveBeenCalledTimes(3);
    const settled = await Promise.race([result.then(() => "settled"), Promise.resolve("pending")]);
    expect(settled).toBe("pending");
  });

  it("rethrows when the page was already reloaded once", async () => {
    const load = vi.fn().mockRejectedValue(new Error("gone"));
    await expect(loadWithRetry(load, { delaysMs: [0, 0], reload: () => false })).rejects.toThrow(
      "gone",
    );
  });
});

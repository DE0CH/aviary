// @vitest-environment jsdom

import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const calls = vi.hoisted(() => ({ api: vi.fn() }));

vi.mock("@/yyc/api", () => {
  class ApiError extends Error {
    constructor(
      message: string,
      readonly status: number,
      readonly code: string | null,
    ) {
      super(message);
    }
  }
  return { api: calls.api, ApiError };
});

import { ApiError } from "@/yyc/api";
import { MESSAGE_SEARCH_DEBOUNCE_MS } from "./message-search-model";
import { useMessageSearch } from "./message-search";

const hit = {
  conversationId: "c1",
  title: "Shuttle bus",
  epoch: "1",
  seq: 9,
  kind: "assistant",
  at: "2026-11-17T08:01:02.000Z",
  snippet: "The shuttle bus leaves every 15 minutes.",
  matched: "meaning",
};

async function settle(ms = MESSAGE_SEARCH_DEBOUNCE_MS + 10) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

describe("useMessageSearch", () => {
  it("shows the words' hits at once and the meaning's when they come, never the slower one first", async () => {
    let finish: (v: unknown) => void = () => undefined;
    const meaningHit = { ...hit, seq: 11, matched: "meaning" };
    calls.api.mockImplementation((_path: string, init: RequestInit) => {
      const body = JSON.parse(String(init.body)) as {
        query: string;
        meaning?: boolean;
      };
      if (body.meaning === false)
        return Promise.resolve({
          query: body.query,
          hits: [hit],
          semantic: false,
        });
      return new Promise((resolve) => {
        finish = resolve;
      });
    });
    const { result, rerender } = renderHook(
      ({ query }) => useMessageSearch({ enabled: true, query }),
      {
        initialProps: { query: "" },
      },
    );
    rerender({ query: "shuttle" });
    await settle();
    expect(result.current.hits).toEqual([hit]);
    expect(result.current.loading).toBe(true);
    await act(async () =>
      finish({ query: "shuttle", hits: [hit, meaningHit], semantic: true }),
    );
    expect(result.current.hits).toEqual([hit, meaningHit]);
    expect(result.current.loading).toBe(false);
  });

  beforeEach(() => {
    vi.useFakeTimers();
    calls.api.mockReset();
  });
  afterEach(() => vi.useRealTimers());

  it("asks the server for the typed query once typing pauses, and shows its hits", async () => {
    calls.api.mockImplementation(async (_path: string, init: RequestInit) => {
      const { query } = JSON.parse(String(init.body)) as { query: string };
      return { query, hits: [hit], semantic: true };
    });
    const { result, rerender } = renderHook(
      ({ query }) => useMessageSearch({ enabled: true, query }),
      {
        initialProps: { query: "" },
      },
    );
    rerender({ query: "sh" });
    rerender({ query: "shu" });
    rerender({ query: "shuttle" });
    await settle();
    // by words at once, then words and meaning
    expect(calls.api).toHaveBeenCalledTimes(2);
    expect(calls.api.mock.calls[0]?.[0]).toBe("/api/jarvis/search");
    expect(
      calls.api.mock.calls.map((c) => JSON.parse(String(c[1]?.body))),
    ).toEqual([
      { query: "shuttle", limit: 20, meaning: false },
      { query: "shuttle", limit: 20 },
    ]);
    expect(result.current.hits).toEqual([hit]);
    expect(result.current.loading).toBe(false);
    // a query too short to search shows nothing and asks nothing
    rerender({ query: "s" });
    expect(result.current.hits).toEqual([]);
    await settle();
    expect(calls.api).toHaveBeenCalledTimes(2);
  });

  it("asks nothing while the palette is closed, and shows no hits after a failure", async () => {
    calls.api.mockRejectedValue(new ApiError("down", 502, "SEARCH_FAILED"));
    const { result, rerender } = renderHook(
      ({ enabled }) => useMessageSearch({ enabled, query: "shuttle" }),
      { initialProps: { enabled: false } },
    );
    await settle();
    expect(calls.api).not.toHaveBeenCalled();
    rerender({ enabled: true });
    await settle();
    expect(calls.api).toHaveBeenCalledTimes(2);
    expect(result.current).toEqual({
      query: "shuttle",
      hits: [],
      loading: false,
    });
  });
});

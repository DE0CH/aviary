import React, { act, Component, type ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { JSDOM } from "jsdom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ChunkErrorBoundary, ChunkLoadFailed } from "./loading-frame";

let root: Root | null = null;

class Outer extends Component<{ children: ReactNode }, { caught: string | null }> {
  state = { caught: null as string | null };
  static getDerivedStateFromError(error: unknown) {
    return { caught: error instanceof Error ? error.message : String(error) };
  }
  render() {
    return this.state.caught ? <p data-testid="outer">{this.state.caught}</p> : this.props.children;
  }
}

function Throws({ error }: { error: Error }): null {
  throw error;
}

beforeEach(() => {
  const dom = new JSDOM("<!doctype html><html><body></body></html>");
  vi.stubGlobal("React", React);
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  vi.stubGlobal("window", dom.window);
  vi.stubGlobal("document", dom.window.document);
  vi.stubGlobal("HTMLElement", dom.window.HTMLElement);
  vi.stubGlobal("Node", dom.window.Node);
  vi.stubGlobal("navigator", dom.window.navigator);
  vi.spyOn(console, "error").mockImplementation(() => undefined);
  const container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root?.unmount());
  root = null;
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe("ChunkErrorBoundary", () => {
  it("keeps a chunk that could not be fetched local, showing its fallback", () => {
    const error = new Error("Loading module /x.js failed.\n(error: /x.js)");
    error.name = "AsyncRequireError";
    act(() => {
      root?.render(
        <Outer>
          <ChunkErrorBoundary name="reply" fallback={<p data-testid="fallback" />}>
            <Throws error={error} />
          </ChunkErrorBoundary>
        </Outer>,
      );
    });
    expect(document.querySelector('[data-testid="fallback"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="outer"]')).toBeNull();
  });

  it("passes a render bug on to the boundary above it", () => {
    act(() => {
      root?.render(
        <Outer>
          <ChunkErrorBoundary name="reply" fallback={<p data-testid="fallback" />}>
            <Throws error={new TypeError("bad timeline item")} />
          </ChunkErrorBoundary>
        </Outer>,
      );
    });
    expect(document.querySelector('[data-testid="fallback"]')).toBeNull();
    expect(document.querySelector('[data-testid="outer"]')?.textContent).toBe("bad timeline item");
  });

  it("can show a Reload control when the chunk could not be fetched", () => {
    const error = new Error("Loading chunk 12 failed.");
    error.name = "ChunkLoadError";
    act(() => {
      root?.render(
        <ChunkErrorBoundary name="workspace" fallback={<ChunkLoadFailed />}>
          <Throws error={error} />
        </ChunkErrorBoundary>,
      );
    });
    expect(document.querySelector('[data-testid="chunk-load-failed"]')).not.toBeNull();
  });
});

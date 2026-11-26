// @vitest-environment jsdom
import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

vi.mock("react-native", () => ({ BackHandler: { addEventListener: vi.fn() }, Platform: { OS: "android" } }));

const { useSidePanelBack } = await import("./side-panel-back");

/** A Back button the test presses: what the latest subscriber answers (true: it took the press). */
function backButton() {
  const handlers: Array<() => boolean> = [];
  return {
    source: {
      subscribe(onBackPress: () => boolean) {
        handlers.push(onBackPress);
        return () => handlers.splice(handlers.indexOf(onBackPress), 1);
      },
    },
    press: (): boolean => handlers.at(-1)?.() ?? false,
  };
}

describe("useSidePanelBack", () => {
  it("Back closes a side panel on screen and is taken", () => {
    const back = backButton();
    const close = vi.fn();
    renderHook(() => useSidePanelBack(true, close, back.source));
    expect(back.press()).toBe(true);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it("with no panel on screen Back is the system's", () => {
    const back = backButton();
    const close = vi.fn();
    renderHook(() => useSidePanelBack(false, close, back.source));
    expect(back.press()).toBe(false);
    expect(close).not.toHaveBeenCalled();
  });

  it("lets Back go once the panel is closed", () => {
    const back = backButton();
    const close = vi.fn();
    const { rerender } = renderHook(({ open }) => useSidePanelBack(open, close, back.source), {
      initialProps: { open: true },
    });
    rerender({ open: false });
    expect(back.press()).toBe(false);
    expect(close).not.toHaveBeenCalled();
  });
});

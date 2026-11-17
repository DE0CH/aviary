/**
 * @vitest-environment jsdom
 */
import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const platform = vi.hoisted(() => ({ isNative: true }));
const splash = vi.hoisted(() => ({ hide: vi.fn() }));
vi.mock("@/constants/platform", () => platform);
vi.mock("expo-splash-screen", () => splash);

import { useHideLaunchScreen } from "./launch-screen";

describe("useHideLaunchScreen", () => {
  beforeEach(() => {
    platform.isNative = true;
    splash.hide.mockClear();
  });

  it("hides iOS's launch screen when the sign-in screen shows (the router, which hides it otherwise, is not mounted)", () => {
    const { rerender } = renderHook(({ when }) => useHideLaunchScreen(when), {
      initialProps: { when: false },
    });
    expect(splash.hide).not.toHaveBeenCalled();
    rerender({ when: true });
    expect(splash.hide).toHaveBeenCalledTimes(1);
  });

  it("does nothing on the web", () => {
    platform.isNative = false;
    renderHook(() => useHideLaunchScreen(true));
    expect(splash.hide).not.toHaveBeenCalled();
  });

  it("a build without the native module is not an error", () => {
    splash.hide.mockImplementationOnce(() => {
      throw new Error("no ExpoSplashScreen");
    });
    expect(() => renderHook(() => useHideLaunchScreen(true))).not.toThrow();
  });
});

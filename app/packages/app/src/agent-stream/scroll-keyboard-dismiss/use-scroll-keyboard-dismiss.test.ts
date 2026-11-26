// @vitest-environment jsdom
import { renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

// The flick that puts the keyboard away, from the list's scroll events to the keyboard, with exact event times: the
// Android emulator's walk no longer flicks (its injected swipes arrive in uneven bursts, so the speed it measured
// depended on the runner; app/scripts/android-app/emulator-test.mjs).

const keyboard = vi.hoisted(() => ({
  dismiss: vi.fn(),
  focused: null as unknown,
  blur: vi.fn(),
  shift: { value: 300 },
}));

vi.mock("react-native", () => ({
  Keyboard: { dismiss: keyboard.dismiss },
  TextInput: { State: { currentlyFocusedInput: () => keyboard.focused, blurTextInput: keyboard.blur } },
}));
vi.mock("@/keyboard/shift", () => ({ useKeyboardShift: () => ({ shift: keyboard.shift }) }));

const { useScrollKeyboardDismiss } = await import("./use-scroll-keyboard-dismiss");

/** A scroll event at `timestamp` ms with the list at `y`. */
function at(timestamp: number, y: number) {
  return { timeStamp: timestamp, nativeEvent: { contentOffset: { y }, timestamp } } as never;
}

/** A drag from y 0 at t 0 to `y` at `ms`, sampled every 10 ms, then released there. */
function drag(hook: ReturnType<typeof useScrollKeyboardDismiss>, y: number, ms: number) {
  hook.onScrollBeginDrag(at(1000, 0));
  for (let t = 10; t < ms; t += 10) hook.onScroll(at(1000 + t, (y * t) / ms));
  hook.onScrollEndDrag(at(1000 + ms, y));
}

describe("useScrollKeyboardDismiss", () => {
  beforeEach(() => {
    keyboard.dismiss.mockClear();
    keyboard.blur.mockClear();
    keyboard.focused = { input: "message box" };
    keyboard.shift.value = 300;
  });

  it("a fast flick blurs the message box and puts the keyboard away", () => {
    const { result } = renderHook(() => useScrollKeyboardDismiss());
    drag(result.current, 400, 100); // 4 points/ms
    expect(keyboard.blur).toHaveBeenCalledWith(keyboard.focused);
    expect(keyboard.dismiss).toHaveBeenCalledTimes(1);
  });

  it("a slow read-scroll keeps the keyboard", () => {
    const { result } = renderHook(() => useScrollKeyboardDismiss());
    drag(result.current, 400, 1000); // 0.4 points/ms
    expect(keyboard.dismiss).not.toHaveBeenCalled();
    expect(keyboard.blur).not.toHaveBeenCalled();
  });

  it("a flick with no on-screen keyboard leaves a focused box alone (a hardware keyboard)", () => {
    keyboard.shift.value = 0;
    const { result } = renderHook(() => useScrollKeyboardDismiss());
    drag(result.current, 400, 100);
    expect(keyboard.dismiss).not.toHaveBeenCalled();
    expect(keyboard.blur).not.toHaveBeenCalled();
  });
});

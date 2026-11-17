import { beforeEach, describe, expect, it, vi } from "vitest";

const platform = vi.hoisted(() => ({ isNative: true }));
const haptics = vi.hoisted(() => ({
  impactAsync: vi.fn(async () => {}),
  notificationAsync: vi.fn(async () => {}),
  ImpactFeedbackStyle: { Light: "light", Medium: "medium" },
  NotificationFeedbackType: { Success: "success" },
}));
vi.mock("@/constants/platform", () => platform);
vi.mock("expo-haptics", () => haptics);

import { playChatHaptic, replyEnded } from "./haptics";

describe("playChatHaptic", () => {
  beforeEach(() => {
    platform.isNative = true;
    haptics.impactAsync.mockClear();
    haptics.notificationAsync.mockClear();
  });

  it("taps lightly for a send, firmer for a stop, and plays the system's done for a reply's end", () => {
    playChatHaptic("send");
    expect(haptics.impactAsync).toHaveBeenLastCalledWith("light");
    playChatHaptic("stop");
    expect(haptics.impactAsync).toHaveBeenLastCalledWith("medium");
    playChatHaptic("reply-ended");
    expect(haptics.notificationAsync).toHaveBeenLastCalledWith("success");
  });

  it("does nothing on the web", () => {
    platform.isNative = false;
    playChatHaptic("send");
    playChatHaptic("reply-ended");
    expect(haptics.impactAsync).not.toHaveBeenCalled();
    expect(haptics.notificationAsync).not.toHaveBeenCalled();
  });

  it("a phone that cannot tap is not an error", async () => {
    haptics.impactAsync.mockRejectedValueOnce(new Error("no engine"));
    expect(() => playChatHaptic("send")).not.toThrow();
    await Promise.resolve();
  });
});

describe("replyEnded", () => {
  it("is true only when a running reply stops", () => {
    expect(replyEnded(true, false)).toBe(true);
    expect(replyEnded(false, false)).toBe(false);
    expect(replyEnded(true, true)).toBe(false);
    expect(replyEnded(false, true)).toBe(false);
  });
});

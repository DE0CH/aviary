import { describe, expect, it } from "vitest";
import { conversationMenuHasItems } from "./conversation-menu";

const act = () => {};

describe("a conversation's ⋯ menu in the list", () => {
  it("is there without delete when it can rename, pin or share (Jarvis chat)", () => {
    expect(conversationMenuHasItems({ onRename: act, canShare: false })).toBe(true);
    expect(conversationMenuHasItems({ onTogglePin: act, canShare: false })).toBe(true);
    expect(conversationMenuHasItems({ canShare: true })).toBe(true);
  });

  it("is there with delete alone", () => {
    expect(conversationMenuHasItems({ onArchive: act, canShare: false })).toBe(true);
  });

  it("is not there when it would be empty", () => {
    expect(conversationMenuHasItems({ canShare: false })).toBe(false);
  });
});

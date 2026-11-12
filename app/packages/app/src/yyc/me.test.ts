import { describe, expect, it } from "vitest";
import { meOf } from "./me";

describe("meOf", () => {
  it("offers what every server has unless the server says no", () => {
    expect(meOf({ firstName: "Tai" })).toEqual({
      firstName: "Tai",
      sharing: true,
      newConversations: true,
      conversationDelete: true,
      midTurn: false,
      modelPick: false,
    });
  });

  it("a deployment that starts and ends every conversation itself offers no new one and no delete", () => {
    const me = meOf({ newConversations: false, conversationDelete: false, midTurn: true });
    expect(me.newConversations).toBe(false);
    expect(me.conversationDelete).toBe(false);
    expect(me.midTurn).toBe(true);
  });

  it("reads an empty answer as the defaults", () => {
    expect(meOf(null).conversationDelete).toBe(true);
    expect(meOf(undefined).firstName).toBeNull();
  });
});

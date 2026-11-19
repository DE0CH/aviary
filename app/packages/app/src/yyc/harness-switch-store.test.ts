import { describe, expect, it } from "vitest";
import { MOVE_WAITS_MS, moveAfter, useHarnessSwitchStore, type Move } from "./harness-switch-store";

describe("a conversation moved to another harness", () => {
  const waiting: Move = { phase: "waiting", at: 1_000 };

  it("is summed up during its next turn, and only that one", () => {
    expect(moveAfter(null, true, 2_000)).toBeNull();
    expect(moveAfter(waiting, false, 2_000)).toBe(waiting);
    const running = moveAfter(waiting, true, 2_000);
    expect(running).toEqual({ phase: "running", at: 1_000 });
    expect(moveAfter(running, true, 3_000)).toBe(running);
    expect(moveAfter(running, false, 4_000)).toBeNull();
  });

  it("is forgotten when no message follows", () => {
    expect(moveAfter(waiting, false, 1_000 + MOVE_WAITS_MS + 1)).toBeNull();
  });

  it("is made only on Continue", () => {
    const picks: string[] = [];
    const store = useHarnessSwitchStore;
    store.getState().ask({ agentId: "a1", modelLabel: "MiMo", confirm: () => picks.push("a1") });
    store.getState().answer(false);
    expect(picks).toEqual([]);
    expect(store.getState().question).toBeNull();
    expect(store.getState().moves.a1).toBeUndefined();

    store.getState().ask({ agentId: "a1", modelLabel: "MiMo", confirm: () => picks.push("a1") });
    store.getState().answer(true);
    expect(picks).toEqual(["a1"]);
    expect(store.getState().moves.a1?.phase).toBe("waiting");
    store.getState().seen("a1", true);
    expect(store.getState().moves.a1?.phase).toBe("running");
    store.getState().seen("a1", false);
    expect(store.getState().moves.a1).toBeUndefined();
  });
});

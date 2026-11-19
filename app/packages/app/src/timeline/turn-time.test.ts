import assert from "node:assert/strict";
import { describe, it } from "vitest";
import { deriveStreamTurnTiming } from "./turn-time";
import type { StreamItem } from "@/types/stream";

function user(id: string, timestamp: Date): StreamItem {
  return {
    kind: "user_message",
    id,
    text: id,
    timestamp,
  };
}

function assistant(id: string, timestamp: Date, text = id): StreamItem {
  return {
    kind: "assistant_message",
    id,
    text,
    timestamp,
  };
}

describe("deriveStreamTurnTiming", () => {
  it("starts elapsed time from the submitted prompt", () => {
    const submittedAt = new Date("2026-05-15T00:00:00.000Z");

    const timing = deriveStreamTurnTiming({
      isTurnActive: true,
      activeTurnStartedAt: submittedAt,
      tail: [],
      head: [user("submitted", submittedAt)],
    });

    assert.equal(timing.runningStartedAt, submittedAt);
  });

  it("uses the last user message as the running turn start", () => {
    const firstUserAt = new Date("2026-05-15T00:00:00.000Z");
    const secondUserAt = new Date("2026-05-15T00:01:00.000Z");

    const timing = deriveStreamTurnTiming({
      isTurnActive: true,
      activeTurnStartedAt: secondUserAt,
      tail: [
        user("u1", firstUserAt),
        assistant("a1", new Date("2026-05-15T00:00:05.000Z")),
        user("u2", secondUserAt),
      ],
      head: [assistant("a2", new Date("2026-05-15T00:01:04.000Z"))],
    });

    assert.equal(timing.runningStartedAt, secondUserAt);
    assert.equal(timing.byAssistantId.has("a2"), false);
  });

  it("derives completed turn timing from user and assistant item timestamps", () => {
    const userAt = new Date("2026-05-15T00:00:00.000Z");
    const assistantAt = new Date("2026-05-15T00:00:07.000Z");

    const timing = deriveStreamTurnTiming({
      isTurnActive: false,
      activeTurnStartedAt: null,
      tail: [
        user("u1", userAt),
        assistant("a1", assistantAt),
        user("u2", new Date("2026-05-15T00:01:00.000Z")),
      ],
      head: [],
    });

    assert.deepEqual(timing.byAssistantId.get("a1"), {
      completedAt: assistantAt,
      durationMs: 7000,
    });
  });

  it("maps multiple assistant chunks in one turn to the same timing", () => {
    const userAt = new Date("2026-05-15T00:00:00.000Z");
    const firstAssistantAt = new Date("2026-05-15T00:00:03.000Z");
    const lastAssistantAt = new Date("2026-05-15T00:00:07.000Z");

    const timing = deriveStreamTurnTiming({
      isTurnActive: false,
      activeTurnStartedAt: null,
      tail: [
        user("u1", userAt),
        assistant("a1", firstAssistantAt),
        assistant("a2", lastAssistantAt),
      ],
      head: [],
    });

    const expected = {
      completedAt: lastAssistantAt,
      durationMs: 7000,
    };
    assert.deepEqual(timing.byAssistantId.get("a1"), expected);
    assert.deepEqual(timing.byAssistantId.get("a2"), expected);
  });

  it("preserves the completion timestamp when a canonical turn has no visible prompt", () => {
    const firstTurnAt = new Date("2026-05-15T00:00:00.000Z");
    const hiddenPromptTurnAt = new Date("2026-05-15T00:01:07.000Z");
    const timing = deriveStreamTurnTiming({
      isTurnActive: false,
      activeTurnStartedAt: null,
      tail: [
        { ...user("u1", firstTurnAt), turnId: "turn-1" },
        {
          ...assistant("a1", new Date("2026-05-15T00:00:07.000Z")),
          turnId: "turn-1",
        },
        {
          ...assistant("hidden-prompt-a1", new Date("2026-05-15T00:01:03.000Z")),
          turnId: "turn-2",
        },
        { ...assistant("hidden-prompt-a2", hiddenPromptTurnAt), turnId: "turn-2" },
      ],
      head: [],
    });

    assert.deepEqual(timing.byAssistantId.get("hidden-prompt-a2"), {
      completedAt: hiddenPromptTurnAt,
      durationMs: null,
    });
  });

  it("keeps the timing map identity while a streamed delta grows the live head", () => {
    const tail = [
      user("u1", new Date("2026-05-15T00:00:00.000Z")),
      assistant("a1", new Date("2026-05-15T00:00:07.000Z")),
      user("u2", new Date("2026-05-15T00:01:00.000Z")),
    ];
    const derive = (text: string, at: string) =>
      deriveStreamTurnTiming({
        isTurnActive: true,
        activeTurnStartedAt: tail[2]?.timestamp ?? null,
        tail,
        head: [assistant("a2", new Date(at), text)],
      });

    const first = derive("one", "2026-05-15T00:01:04.000Z");
    const second = derive("one two", "2026-05-15T00:01:05.000Z");

    assert.equal(second.byAssistantId, first.byAssistantId);
    assert.deepEqual(first.byAssistantId.get("a1"), {
      completedAt: tail[1]?.timestamp,
      durationMs: 7000,
    });
    assert.equal(first.byAssistantId.has("a2"), false);
  });

  it("keeps the timing map identity when the head closed the tail's open turn", () => {
    const lastTailAt = new Date("2026-05-15T00:00:07.000Z");
    const tail = [user("u1", new Date("2026-05-15T00:00:00.000Z")), assistant("a1", lastTailAt)];
    const derive = (text: string, at: string) =>
      deriveStreamTurnTiming({
        isTurnActive: true,
        activeTurnStartedAt: null,
        tail,
        head: [
          user("u2", new Date("2026-05-15T00:01:00.000Z")),
          assistant("a2", new Date(at), text),
        ],
      });

    const first = derive("one", "2026-05-15T00:01:04.000Z");
    const second = derive("one two", "2026-05-15T00:01:05.000Z");

    assert.equal(second.byAssistantId, first.byAssistantId);
    assert.deepEqual(first.byAssistantId.get("a1"), {
      completedAt: lastTailAt,
      durationMs: 7000,
    });
  });

  it("produces a new timing map when the streamed turn completes", () => {
    const tail = [user("u1", new Date("2026-05-15T00:00:00.000Z"))];
    const completedAt = new Date("2026-05-15T00:00:09.000Z");
    const head = [assistant("a1", completedAt)];

    const active = deriveStreamTurnTiming({
      isTurnActive: true,
      activeTurnStartedAt: tail[0]?.timestamp ?? null,
      tail,
      head,
    });
    const completed = deriveStreamTurnTiming({
      isTurnActive: false,
      activeTurnStartedAt: null,
      tail,
      head,
    });

    assert.notEqual(completed.byAssistantId, active.byAssistantId);
    assert.equal(active.byAssistantId.has("a1"), false);
    assert.deepEqual(completed.byAssistantId.get("a1"), { completedAt, durationMs: 9000 });
  });

  it("derives fresh timing when the tail itself changes", () => {
    const firstTail = [
      user("u1", new Date("2026-05-15T00:00:00.000Z")),
      assistant("a1", new Date("2026-05-15T00:00:07.000Z")),
      user("u2", new Date("2026-05-15T00:01:00.000Z")),
    ];
    const secondTail = [...firstTail, assistant("a2", new Date("2026-05-15T00:01:03.000Z"))];
    const derive = (tail: StreamItem[]) =>
      deriveStreamTurnTiming({ isTurnActive: false, activeTurnStartedAt: null, tail, head: [] });

    const first = derive(firstTail);
    const second = derive(secondTail);

    assert.notEqual(second.byAssistantId, first.byAssistantId);
    assert.equal(first.byAssistantId.has("a2"), false);
    assert.deepEqual(second.byAssistantId.get("a2"), {
      completedAt: secondTail[3]?.timestamp,
      durationMs: 3000,
    });
  });

  describe("question waits (YYC DECISIONS 17)", () => {
    const t = (clock: string) => new Date(`2026-11-06T${clock}.000Z`);
    function question(
      id: string,
      timestamp: Date,
      opts: { name?: string; status?: "running" | "completed" | "failed" | "canceled"; answeredAt?: string; turnId?: string } = {},
    ): StreamItem {
      return {
        kind: "tool_call",
        id,
        timestamp,
        ...(opts.turnId ? { turnId: opts.turnId } : {}),
        payload: {
          source: "agent",
          data: {
            provider: "claude",
            callId: id,
            name: opts.name ?? "AskUserQuestion",
            status: opts.status ?? "completed",
            error: null,
            detail: { type: "unknown", input: null, output: null },
            ...(opts.answeredAt ? { metadata: { answeredAt: opts.answeredAt } } : {}),
          },
        },
      };
    }
    const derive = (items: StreamItem[]) =>
      deriveStreamTurnTiming({ isTurnActive: false, activeTurnStartedAt: null, tail: items, head: [] });

    it("leaves the wait for an answer out of the working time (answeredAt)", () => {
      // R1 C: asked 13:36:10, answered 42 minutes later, the reply 20 s after the answer
      const timing = derive([
        user("u", t("05:36:00")),
        question("q", t("05:36:10"), { answeredAt: "2026-11-06T06:18:37.000Z" }),
        assistant("a", t("06:18:57")),
      ]);
      assert.equal(timing.byAssistantId.get("a")?.durationMs, 30_000);
    });

    it("counts each answered question of the turn, and a plan approval too", () => {
      const timing = derive([
        user("u", t("01:00:00")),
        question("q1", t("01:00:05"), { answeredAt: "2026-11-06T01:10:05.000Z" }),
        assistant("a1", t("01:10:07")),
        question("q2", t("01:10:10"), { name: "plan_approval", answeredAt: "2026-11-06T02:00:00.000Z" }),
        assistant("a2", t("02:00:03")),
      ]);
      // 5 s + 2 s + 3 s + 3 s of work
      assert.equal(timing.byAssistantId.get("a2")?.durationMs, 13_000);
    });

    it("an answered question without an answer time waits until the turn's next item", () => {
      const timing = derive([
        user("u", t("03:00:00")),
        assistant("a0", t("03:00:04")),
        question("q", t("03:00:06")),
        assistant("a", t("03:45:00")),
      ]);
      assert.equal(timing.byAssistantId.get("a")?.durationMs, 6_000);
    });

    it("a question still open, or one that is not a question, takes nothing off", () => {
      const open = derive([
        user("u", t("04:00:00")),
        question("q", t("04:00:02"), { status: "running" }),
        assistant("a", t("04:00:09")),
      ]);
      assert.equal(open.byAssistantId.get("a")?.durationMs, 9_000);
      const bash = derive([
        user("u", t("04:00:00")),
        question("b", t("04:00:02"), { name: "Bash" }),
        assistant("a", t("04:05:00")),
      ]);
      assert.equal(bash.byAssistantId.get("a")?.durationMs, 300_000);
    });

    it("a wait never counts past the turn's last item, nor into the next turn", () => {
      const timing = derive([
        user("u1", t("05:00:00")),
        assistant("a1", t("05:00:04")),
        question("q", t("05:00:06"), { status: "canceled", answeredAt: "2026-11-06T06:00:00.000Z" }),
        user("u2", t("05:30:00")),
        assistant("a2", t("05:30:05")),
      ]);
      assert.equal(timing.byAssistantId.get("a1")?.durationMs, 6_000);
      assert.equal(timing.byAssistantId.get("a2")?.durationMs, 5_000);
    });

    it("a turn resumed under the same turn id after its question keeps one footer without the wait", () => {
      const timing = derive([
        { ...user("u", t("06:00:00")), turnId: "T" },
        question("q", t("06:00:03"), { turnId: "T", answeredAt: "2026-11-06T07:00:00.000Z" }),
        { ...assistant("a", t("07:00:10")), turnId: "T" },
      ]);
      assert.equal(timing.byAssistantId.get("a")?.durationMs, 13_000);
    });

    it("the head walked from the tail's open turn carries the wait", () => {
      const tail = [user("u", t("08:00:00")), question("q", t("08:00:02"), { answeredAt: "2026-11-06T08:30:00.000Z" })];
      const timing = deriveStreamTurnTiming({
        isTurnActive: false,
        activeTurnStartedAt: null,
        tail,
        head: [assistant("a", t("08:30:04"))],
      });
      assert.equal(timing.byAssistantId.get("a")?.durationMs, 6_000);
    });
  });
});

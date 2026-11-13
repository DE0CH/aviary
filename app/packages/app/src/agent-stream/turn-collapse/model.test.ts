import { describe, expect, it } from "vitest";
import type { StreamItem, ToolCallItem } from "@/types/stream";
import { collapseTurns, type CollapseTurnsInput } from "./model";

const at = new Date("2026-01-01T00:00:00.000Z");

function user(id: string): StreamItem {
  return { kind: "user_message", id, text: id, timestamp: at };
}

function reply(id: string): StreamItem {
  return { kind: "assistant_message", id, text: id, timestamp: at };
}

function thought(id: string): StreamItem {
  return { kind: "thought", id, text: id, timestamp: at, status: "ready" };
}

function shell(id: string): ToolCallItem {
  return {
    kind: "tool_call",
    id,
    timestamp: at,
    payload: {
      source: "agent",
      data: {
        provider: "claude",
        callId: id,
        name: "Bash",
        status: "completed",
        error: null,
        detail: { type: "shell", command: "ls" },
      },
    },
  };
}

function collapse(tail: StreamItem[], options: Partial<CollapseTurnsInput> = {}) {
  return collapseTurns({
    tail,
    isLatestTurnOpen: false,
    expandedHostIds: new Set(),
    getGroupedCalls: () => undefined,
    ...options,
  });
}

const ids = (items: StreamItem[]) => items.map((item) => item.id);

describe("collapseTurns", () => {
  it("keeps the AI's schedules call (its card) under the reply when the turn folds", () => {
    const armed: ToolCallItem = {
      ...shell("s1"),
      payload: {
        source: "agent",
        data: {
          provider: "claude",
          callId: "s1",
          name: "Bash",
          status: "completed",
          error: null,
          detail: {
            type: "shell",
            command: 'B=$JARVIS_ROUTE_SCHEDULES_URL/schedules\nS -X POST "$B" -d \'{"at": "2026-11-06T14:42:00+08:00"}\'',
            output: '{"id":"sch_1","kind":"once","prompt":"p","tz":"Asia/Hong_Kong","describe":"11月6日 14:42","next":null,"state":"done"}',
          },
        },
      },
    };
    const result = collapse([user("u1"), shell("c1"), armed, reply("final")]);
    expect(ids(result.tail)).toEqual(["u1", "final", "s1"]);
    expect(result.turnsByHostId.get("u1")?.summary.tools).toEqual(expect.objectContaining({ commandCount: 1 }));
  });

  it("folds a finished turn's work into its user message and keeps the reply", () => {
    const result = collapse([
      user("u1"),
      thought("t1"),
      reply("early"),
      shell("c1"),
      shell("c2"),
      reply("final"),
    ]);

    expect(ids(result.tail)).toEqual(["u1", "final"]);
    expect(result.turnsByHostId.get("u1")).toEqual({
      hostId: "u1",
      expanded: false,
      summary: {
        messageCount: 1,
        tools: expect.objectContaining({ commandCount: 2 }),
      },
    });
    expect(result.hostIdByFoldedMessageId.get("early")).toBe("u1");
  });

  it("shows an expanded turn's work as nested rows between the prompt and the reply", () => {
    const result = collapse([user("u1"), thought("t1"), shell("c1"), reply("final")], {
      expandedHostIds: new Set(["u1"]),
    });

    expect(ids(result.tail)).toEqual(["u1", "t1", "c1", "final"]);
    expect([...result.nestedItemIds]).toEqual(["t1", "c1"]);
    expect(result.turnsByHostId.get("u1")?.expanded).toBe(true);
  });

  it("leaves the latest turn alone while it is still producing output", () => {
    const tail = [user("u1"), shell("c1"), reply("r1"), user("u2"), shell("c2"), reply("r2")];

    const result = collapse(tail, { isLatestTurnOpen: true });

    expect(ids(result.tail)).toEqual(["u1", "r1", "u2", "c2", "r2"]);
    expect([...result.turnsByHostId.keys()]).toEqual(["u1"]);
  });

  it("keeps rows after the reply, and turns with no work before the reply, as they are", () => {
    const result = collapse([
      user("u1"),
      shell("c1"),
      reply("r1"),
      shell("after"),
      user("u2"),
      reply("r2"),
    ]);

    expect(ids(result.tail)).toEqual(["u1", "r1", "after", "u2", "r2"]);
    expect([...result.turnsByHostId.keys()]).toEqual(["u1"]);
  });

  it("does not fold a turn that only thought before its reply", () => {
    const tail = [user("u1"), thought("t1"), reply("r1")];

    const result = collapse(tail);

    expect(result.tail).toBe(tail);
    expect(result.turnsByHostId.size).toBe(0);
  });

  it("does not fold a turn without a reply", () => {
    const tail = [user("u1"), shell("c1")];

    const result = collapse(tail);

    expect(result.tail).toBe(tail);
    expect(result.turnsByHostId.size).toBe(0);
  });

  // Deyao, 2026-11-13: steps folded into "used N tools" only when the whole turn was loaded. A long turn's prompt is
  // pages above what a conversation opens on, so its steps showed one by one, and the view jumped when the prompt
  // finally loaded and the turn folded.
  it("folds a turn whose prompt is above the loaded history into its reply", () => {
    const tail = [shell("c0"), shell("c00"), reply("r0"), user("u1"), shell("c1"), reply("r1")];

    const result = collapse(tail);

    // the loaded part of the top turn is one line, carried by the reply's row, above it
    expect(ids(result.tail)).toEqual(["r0", "u1", "r1"]);
    expect(result.turnsByHostId.get("r0")).toMatchObject({ hostId: "r0", headless: true, expanded: false });
    const counted = Object.values(result.turnsByHostId.get("r0")?.summary.tools ?? {});
    expect(counted.reduce((sum, count) => sum + count, 0)).toBe(2);
    expect(result.turnsByHostId.get("u1")?.headless).toBeUndefined();
    // expanded: its steps show above the reply, as a turn's do
    const open = collapse(tail, { expandedHostIds: new Set(["r0"]) });
    expect(ids(open.tail)).toEqual(["c0", "c00", "r0", "u1", "r1"]);
    expect(open.nestedItemIds.has("c0")).toBe(true);
  });

  it("leaves a top turn that is still producing output, or has nothing before its reply, as it is", () => {
    const running = [shell("c0"), reply("r0")];
    expect(collapse(running, { isLatestTurnOpen: true }).turnsByHostId.size).toBe(0);
    const onlyReply = [reply("r0"), user("u1")];
    expect(collapse(onlyReply).turnsByHostId.size).toBe(0);
  });

  it("counts every call a tool-call group row stands for", () => {
    const group = [shell("c1"), shell("c2"), shell("c3")];
    const result = collapse([user("u1"), group[0]!, reply("final")], {
      getGroupedCalls: (itemId) => (itemId === "c1" ? group : undefined),
    });

    expect(result.turnsByHostId.get("u1")?.summary).toMatchObject({
      tools: { commandCount: 3 },
    });
  });
});

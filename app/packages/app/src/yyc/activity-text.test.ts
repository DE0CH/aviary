import { describe, expect, it } from "vitest";
import {
  type ActivityTask,
  activityLocale,
  chipLabel,
  elapsedHeading,
  elapsedLabel,
  nowLabel,
  runningCount,
  taskElapsedMs,
  taskTitle,
} from "./activity-text";

const task = (over: Partial<ActivityTask>): ActivityTask => ({
  taskId: "t1",
  type: "command",
  status: "running",
  startedAt: "2026-11-12T08:00:00.000Z",
  ...over,
});

describe("the activity panel's words", () => {
  it("counts what runs and names the chip after it", () => {
    const tasks = [task({}), task({ taskId: "t2", status: "completed" }), task({ taskId: "t3" })];
    expect(runningCount(tasks)).toBe(2);
    expect(chipLabel(tasks, "en")).toBe("2 running");
    expect(chipLabel(tasks, "zh")).toBe("2 個進行中");
    expect(chipLabel([task({ status: "stopped" })], "en")).toBe("Activity");
  });

  it("writes a time as m:ss, and h:mm:ss from an hour on", () => {
    expect(elapsedLabel(0)).toBe("0:00");
    expect(elapsedLabel(65_400)).toBe("1:05");
    expect(elapsedLabel(3_725_000)).toBe("1:02:05");
    expect(elapsedLabel(-5)).toBe("0:00");
  });

  it("measures a running task to now and an ended one to its end", () => {
    const now = Date.parse("2026-11-12T08:04:12.000Z");
    expect(taskElapsedMs(task({}), now)).toBe(252_000);
    expect(taskElapsedMs(task({ endedAt: "2026-11-12T08:00:31.000Z" }), now)).toBe(31_000);
    expect(taskElapsedMs(task({ startedAt: "never" }), now)).toBe(0);
  });

  it("says how a task ended beside its time", () => {
    expect(elapsedHeading("running", "en")).toBe("Running for");
    expect(elapsedHeading("completed", "en")).toBe("Finished in");
    expect(elapsedHeading("failed", "en")).toBe("Failed after");
    expect(elapsedHeading("stopped", "zh")).toBe("已停止，用時");
  });

  it("titles a task by what it was started to do, else by what it is", () => {
    expect(taskTitle(task({ description: " Run the tests " }), "en")).toBe("Run the tests");
    expect(taskTitle(task({ type: "subagent", description: null }), "en")).toBe("Subagent");
    expect(taskTitle(task({ type: "workflow" }), "zh")).toBe("工作流程");
  });

  it("drops Claude Code's own Running prefix from what a task does now", () => {
    expect(nowLabel("Running Read src/lib.rs")).toBe("Read src/lib.rs");
    expect(nowLabel("  ")).toBeNull();
    expect(nowLabel(undefined)).toBeNull();
  });

  it("is Chinese for a Chinese interface and English for every other", () => {
    expect(activityLocale("zh-HK")).toBe("zh");
    expect(activityLocale("en-GB")).toBe("en");
    expect(activityLocale(undefined)).toBe("en");
  });
});

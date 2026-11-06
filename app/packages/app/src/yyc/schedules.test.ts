import { describe, expect, it } from "vitest";
import { isScheduleCallDetail, nextLabel, parseScheduledMessage, readScheduleCall, shortLabel } from "./schedules-text";

describe("schedules in the chat", () => {
  it("reads a fired schedule's message", () => {
    expect(parseScheduledMessage('<scheduled-message rule="每個工作日 08:30">整理新聞</scheduled-message>')).toEqual({
      rule: "每個工作日 08:30",
      prompt: "整理新聞",
    });
    expect(parseScheduledMessage('<scheduled-message rule="a&quot;b&amp;c">x\ny</scheduled-message>')).toEqual({
      rule: 'a"b&c',
      prompt: "x\ny",
    });
    expect(parseScheduledMessage("整理新聞")).toBeNull();
  });

  it("reads the AI's API calls on both stacks", () => {
    const answer =
      '{"id":"sch_0123","conversationId":"c","kind":"cron","prompt":"p","cron":"30 8 * * 1-5","tz":"Asia/Hong_Kong","describe":"每個工作日 08:30","next":"2026-11-09T08:30:00+08:00","state":"active","createdBy":"a@b.c","createdAt":"2026-11-06T09:00:00+08:00"}';
    const newStack = 'S() { curl -sS "$@"; }\nB=http://magpie/v1/sandbox/schedules\nS -X POST "$B" -H \'Content-Type: application/json\' -d \'{"prompt": "p", "cron": "30 8 * * 1-5"}\'';
    expect(readScheduleCall(newStack, answer)?.action).toBe("armed");
    expect(readScheduleCall(newStack, answer)?.schedule?.describe).toBe("每個工作日 08:30");
    const oldStack = newStack.replace("http://magpie/v1/sandbox/schedules", "$JARVIS_ROUTE_SCHEDULES_URL/schedules");
    expect(readScheduleCall(oldStack, `noise\n${answer}\n`)?.action).toBe("armed");
    expect(readScheduleCall(newStack.replace("-X POST", "-X PATCH"), answer)?.action).toBe("changed");
    expect(readScheduleCall(newStack.replace(/-X POST.*/, '-X DELETE "$B/sch_0123"'), "")?.action).toBe("deleted");
    // a list, a call still running, or something else entirely: not a card
    expect(readScheduleCall('B=http://magpie/v1/sandbox/schedules; curl -sS "$B"', `{"schedules": []}`)).toBeNull();
    expect(readScheduleCall(newStack, undefined)).toBeNull();
    expect(readScheduleCall("ls -la", answer)).toBeNull();
  });

  it("tells a schedules call that sets one (never folded into a group) from other commands", () => {
    const command = 'B=http://magpie/v1/sandbox/schedules\nS -X POST "$B" -d \'{"at": "x"}\'';
    const output = '{"id":"sch_1","kind":"once","prompt":"p","tz":"Asia/Hong_Kong","describe":"d","next":null,"state":"done"}';
    expect(isScheduleCallDetail({ type: "shell", command, output })).toBe(true);
    expect(isScheduleCallDetail({ type: "shell", command })).toBe(false);
    expect(isScheduleCallDetail({ type: "shell", command: "ls", output })).toBe(false);
    expect(isScheduleCallDetail({ type: "read", filePath: "/x" })).toBe(false);
    expect(isScheduleCallDetail(null)).toBe(false);
  });

  it("puts times in words", () => {
    expect(shortLabel("2026-11-06T08:30:00+08:00")).toBe("11月6日 08:30");
    expect(nextLabel(null, "Asia/Hong_Kong")).toBe("沒有");
    expect(nextLabel("2099-01-05T15:00:00+08:00", "Asia/Hong_Kong")).toBe("1月5日（星期一）15:00");
  });
});

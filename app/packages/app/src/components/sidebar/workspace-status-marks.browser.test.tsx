// The chat's marks in the conversation list (design A, Deyao 2026-11-17): the status spot of an idle conversation
// shows a blue dot while background tasks run, a clock while a schedule is armed (background tasks win), and the grey
// dot otherwise; a reply in progress keeps the ring.
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { i18n as testI18n } from "@/i18n/i18next";
import { WorkspaceStatusIndicator } from "./workspace-status-indicator";

beforeEach(() => vi.stubGlobal("React", React));

const mounted: { root: Root; container: HTMLDivElement }[] = [];

afterEach(async () => {
  for (const entry of mounted.splice(0)) {
    act(() => entry.root.unmount());
    entry.container.remove();
  }
  await testI18n.changeLanguage("en");
});

function mount(node: React.ReactElement): HTMLElement {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  act(() => root.render(node));
  mounted.push({ root, container });
  const slot = container.querySelector('[data-testid^="workspace-status-indicator-"]');
  if (!(slot instanceof HTMLElement)) throw new Error("no status indicator rendered");
  return slot;
}

const inTwoHours = () => new Date(Date.now() + 2 * 3600_000);

describe("the conversation list's status spot", () => {
  it("is a blue dot while background tasks run, saying how many", () => {
    const slot = mount(<WorkspaceStatusIndicator bucket="done" backgroundTasks={2} workspaceKind="directory" />);
    expect(slot.dataset.testid).toBe("workspace-status-indicator-background");
    expect(slot.getAttribute("aria-label")).toBe("2 background tasks running");
    const dot = slot.firstElementChild as HTMLElement;
    const color = getComputedStyle(dot).backgroundColor;
    // the running colour (blue), never the idle dot's grey
    const [r, g, b] = (color.match(/\d+/g) ?? []).map(Number);
    expect(b).toBeGreaterThan(r + 80);
    expect(b).toBeGreaterThan(g);
    expect(getComputedStyle(dot).opacity).toBe("1");
  });

  it("is a clock while a schedule is armed, saying when", () => {
    const at = inTwoHours();
    const slot = mount(<WorkspaceStatusIndicator bucket="done" nextScheduleAt={at} workspaceKind="directory" />);
    expect(slot.dataset.testid).toBe("workspace-status-indicator-scheduled");
    expect(slot.getAttribute("aria-label")).toMatch(/^Next wakeup .*\d\d:\d\d/);
  });

  it("shows background tasks over the clock when both", () => {
    const slot = mount(
      <WorkspaceStatusIndicator bucket="done" backgroundTasks={1} nextScheduleAt={inTwoHours()} workspaceKind="directory" />,
    );
    expect(slot.dataset.testid).toBe("workspace-status-indicator-background");
    expect(slot.getAttribute("aria-label")).toBe("1 background task running");
  });

  it("keeps the ring while a reply runs and the grey dot when nothing is pending", () => {
    expect(
      mount(<WorkspaceStatusIndicator bucket="running" backgroundTasks={3} nextScheduleAt={inTwoHours()} workspaceKind="directory" />)
        .dataset.testid,
    ).toBe("workspace-status-indicator-running");
    expect(mount(<WorkspaceStatusIndicator bucket="done" workspaceKind="directory" />).dataset.testid).toBe(
      "workspace-status-indicator-done",
    );
  });

  it("speaks the person's language", async () => {
    await testI18n.changeLanguage("zh-HK");
    const slot = mount(<WorkspaceStatusIndicator bucket="done" backgroundTasks={2} workspaceKind="directory" />);
    expect(slot.getAttribute("aria-label")).toBe("2 個背景工作執行中");
  });
});

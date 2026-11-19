import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { ActivityPanel, useActivityPanel } from "./activity";
import { SchedulesPanel, useSchedulesPanel } from "./schedules";

// Deyao, 2026-11-18: on an iPhone the Activity panel's header (back arrow and title) sat under the status bar. A
// panel that covers the screen on a phone starts its header below the safe area's top.

const INSETS = vi.hoisted(() => ({ top: 59, right: 0, bottom: 34, left: 0 }));
vi.mock("react-native-safe-area-context", () => ({ useSafeAreaInsets: () => INSETS }));
vi.mock("@/yyc/api", () => ({ api: async () => ({ tasks: [], schedules: [] }) }));

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("React", React);
  container = document.createElement("div");
  container.style.cssText = "position:fixed;top:0;left:0;width:390px;height:844px;display:flex";
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  useActivityPanel.getState().close();
  useSchedulesPanel.getState().close();
  vi.unstubAllGlobals();
});

const render = (node: React.ReactNode) =>
  act(() => root.render(<QueryClientProvider client={new QueryClient()}>{node}</QueryClientProvider>));
const top = (testId: string) =>
  document.querySelector<HTMLElement>(`[data-testid="${testId}"]`)?.getBoundingClientRect().top ?? -1;

it("Activity on a phone has its back arrow below the status bar", () => {
  useActivityPanel.getState().toggle("s1");
  render(<ActivityPanel sessionId="s1" compact />);
  expect(top("activity-panel")).toBe(0);
  expect(top("activity-back")).toBeGreaterThanOrEqual(INSETS.top);
});

it("Schedules on a phone has its back arrow below the status bar", () => {
  useSchedulesPanel.getState().toggle("s1");
  render(<SchedulesPanel sessionId="s1" compact />);
  expect(top("schedules-panel")).toBe(0);
  expect(top("schedules-back")).toBeGreaterThanOrEqual(INSETS.top);
});

it("the panel beside the chat has no status bar to leave room for", () => {
  useActivityPanel.getState().toggle("s1");
  render(<ActivityPanel sessionId="s1" compact={false} />);
  expect(top("activity-close")).toBeLessThan(INSETS.top);
});

it("a phone panel's last line scrolls clear of the home indicator", () => {
  useSchedulesPanel.getState().toggle("s1");
  render(<SchedulesPanel sessionId="s1" compact />);
  const spacer = document.querySelector<HTMLElement>('[data-testid="schedules-home-indicator"]');
  expect(spacer?.getBoundingClientRect().height).toBe(INSETS.bottom);
});

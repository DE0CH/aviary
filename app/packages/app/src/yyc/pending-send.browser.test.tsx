import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { i18n } from "@/i18n/i18next";
import { PENDING_WAIT_MS } from "./outbox";
import { PendingSendLine } from "./pending-send";

// Deyao, 2026-11-17, design A: a message the server has not confirmed says 「傳送中」 and, after a few seconds,
// that it waits for the network, with 取消 while it can still be taken back.

let root: Root;
let container: HTMLDivElement;
beforeEach(async () => {
  vi.stubGlobal("React", React);
  await i18n.changeLanguage("zh-HK");
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(async () => {
  act(() => root.unmount());
  container.remove();
  await i18n.changeLanguage("en");
  vi.unstubAllGlobals();
});

const text = () => container.querySelector('[data-testid="pending-send-text"]')?.textContent;
const cancel = () => container.querySelector('[data-testid="pending-send-cancel"]');

it("says it is sending, then that it waits for the network, with 取消", async () => {
  const onCancel = vi.fn();
  // a moment before it has waited long enough
  act(() => root.render(<PendingSendLine sinceMs={Date.now() - PENDING_WAIT_MS + 150} onCancel={onCancel} />));
  expect(text()).toBe("傳送中");
  expect(cancel()).toBeNull();
  await act(async () => {
    await new Promise((resolve) => setTimeout(resolve, 400));
  });
  expect(text()).toBe("等待網絡，連線後自動傳送");
  const button = cancel();
  expect(button?.textContent).toBe("取消");
  act(() => (button as HTMLElement).click());
  expect(onCancel).toHaveBeenCalledTimes(1);
});

it("offers no 取消 for a message already on its way", () => {
  act(() => root.render(<PendingSendLine sinceMs={Date.now() - PENDING_WAIT_MS * 2} />));
  expect(text()).toBe("等待網絡，連線後自動傳送");
  expect(cancel()).toBeNull();
});

it("speaks English too", async () => {
  await act(async () => {
    await i18n.changeLanguage("en");
  });
  act(() => root.render(<PendingSendLine sinceMs={Date.now()} />));
  expect(text()).toBe("Sending");
});

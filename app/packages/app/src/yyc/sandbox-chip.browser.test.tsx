import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { DaemonClient } from "@getpaseo/client/internal/daemon-client";
import { i18n } from "@/i18n/i18next";
import { SandboxPreparingChip } from "./sandbox-chip";
import { mountSandboxStatusHandler } from "./sandbox-status";

// The chip above the composer while a conversation's sandbox is made, driven by the server's `yyc.sandbox_status`
// as the app receives it: a start waiting for the person's approval elsewhere (hermit's phase approval: a new
// conversation's session waiting in the person's inbox) says so, and once approved says it prepares the workspace.

let root: Root;
let container: HTMLDivElement;
let send: (payload: { agentId: string; state: "preparing" | "ready"; waiting?: string }) => void;
beforeEach(async () => {
  vi.stubGlobal("React", React);
  await i18n.changeLanguage("zh-HK");
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  const client = {
    on: (_type: string, cb: (m: { payload: unknown }) => void) => {
      send = (payload) => cb({ payload });
      return () => {};
    },
  } as unknown as DaemonClient;
  mountSandboxStatusHandler(client, "srv");
});
afterEach(async () => {
  act(() => send({ agentId: "a1", state: "ready" }));
  act(() => root.unmount());
  container.remove();
  await i18n.changeLanguage("en");
  vi.unstubAllGlobals();
});

const chip = () => container.querySelector('[data-testid="sandbox-preparing"]')?.textContent ?? null;

it("says it waits for the person's approval, then that it prepares the workspace, then goes", () => {
  act(() => root.render(<SandboxPreparingChip serverId="srv" agentId="a1" />));
  expect(chip()).toBeNull();
  act(() => send({ agentId: "a1", state: "preparing", waiting: "approval" }));
  expect(chip()).toBe("正在等待你的批准…");
  act(() => send({ agentId: "a1", state: "preparing" }));
  expect(chip()).toBe("正在準備工作環境…");
  act(() => send({ agentId: "a1", state: "ready" }));
  expect(chip()).toBeNull();
});

it("shows another conversation's chip only there, and in English too", async () => {
  await act(async () => {
    await i18n.changeLanguage("en");
  });
  act(() => root.render(<SandboxPreparingChip serverId="srv" agentId="a1" />));
  act(() => send({ agentId: "a2", state: "preparing", waiting: "approval" }));
  expect(chip()).toBeNull();
  act(() => send({ agentId: "a1", state: "preparing", waiting: "approval" }));
  expect(chip()).toBe("Waiting for your approval…");
  act(() => send({ agentId: "a2", state: "ready" }));
});

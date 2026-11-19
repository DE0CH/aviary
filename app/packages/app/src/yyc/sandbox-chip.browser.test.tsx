import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConnectionState, DaemonClient } from "@getpaseo/client/internal/daemon-client";
import { i18n } from "@/i18n/i18next";
import { SandboxPreparingChip } from "./sandbox-chip";
import { mountSandboxStatusHandler } from "./sandbox-status";

// The chip above the composer while a conversation's sandbox is made, driven by the server's `yyc.sandbox_status`
// as the app receives it: a start waiting for the person's approval elsewhere (hermit's phase approval: a new
// conversation's session waiting in the person's inbox) says so, and once approved says it prepares the workspace.
// A socket that comes back starts from what the server says on it: a `ready` sent while the socket was down (an
// iPhone app in the background) must not leave the chip up for good.

let root: Root;
let container: HTMLDivElement;
let send: (payload: { agentId: string; state: "preparing" | "ready"; waiting?: string }) => void;
let socket: (status: "connected" | "disconnected") => void;
let unmount: () => void;
beforeEach(async () => {
  vi.stubGlobal("React", React);
  await i18n.changeLanguage("zh-HK");
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  let state: ConnectionState = { status: "connected" };
  const listeners = new Set<(s: ConnectionState) => void>();
  socket = (status) => {
    state = { status };
    for (const listener of listeners) listener(state);
  };
  const client = {
    on: (_type: string, cb: (m: { payload: unknown }) => void) => {
      send = (payload) => cb({ payload });
      return () => {};
    },
    subscribeConnectionStatus: (listener: (s: ConnectionState) => void) => {
      listeners.add(listener);
      listener(state);
      return () => listeners.delete(listener);
    },
  } as unknown as DaemonClient;
  unmount = mountSandboxStatusHandler(client, "srv");
});
afterEach(async () => {
  act(() => send({ agentId: "a1", state: "ready" }));
  unmount();
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

it("goes when the socket comes back and the server no longer says it prepares, and stays when it does", () => {
  act(() => root.render(<SandboxPreparingChip serverId="srv" agentId="a1" />));
  act(() => send({ agentId: "a1", state: "preparing" }));
  expect(chip()).toBe("正在準備工作環境…");
  // the app goes to the background: the socket drops and the sandbox's `ready` goes out while it is down
  act(() => socket("disconnected"));
  expect(chip()).toBe("正在準備工作環境…");
  act(() => socket("connected"));
  expect(chip()).toBeNull();
  // a sandbox still being made is said again on the new socket (pigeon, after the conversation list)
  act(() => send({ agentId: "a1", state: "preparing" }));
  act(() => socket("disconnected"));
  act(() => socket("connected"));
  act(() => send({ agentId: "a1", state: "preparing" }));
  expect(chip()).toBe("正在準備工作環境…");
});

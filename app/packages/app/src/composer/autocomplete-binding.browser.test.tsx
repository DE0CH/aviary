import React, { act, type RefObject } from "react";
import { createRoot, type Root } from "react-dom/client";
import { View } from "react-native";
import { PortalProvider } from "@gorhom/portal";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createStore } from "zustand/vanilla";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { i18n as testI18n } from "@/i18n/i18next";
import { FloatingPanelPortalHost } from "@/components/ui/floating-panel-portal";
import { KeyboardShiftProvider } from "@/keyboard/shift";
import { ComposerKeyboardScopeProvider } from "@/composer/keyboard-scope";
import { resolveComposerInputMode, type ComposerInputMode } from "@/composer/input-mode";
import { ComposerRecall } from "@/composer/recall";
import {
  ComposerAutocompleteBinding,
  type ComposerAutocompleteHandle,
} from "./autocomplete-binding";

// Load translations so the list's texts are the real ones.
void testI18n;

/**
 * The message box's `/` list, as a conversation's composer mounts it: the real binding, hook and
 * popover in a real browser, over a server that answers `list_commands_request` the way the chat's
 * does (every entry of kind "command", the agent's own `clear` among them).
 */

const server = vi.hoisted(() => ({
  commands: [] as { name: string; description: string; argumentHint: string; kind: string }[],
  listCommands: vi.fn(),
  getDirectorySuggestions: vi.fn(),
}));

vi.mock("@/runtime/host-runtime", () => ({
  useHostRuntimeClient: () => server,
  useHostRuntimeIsConnected: () => true,
}));

// App sources compile against the classic JSX runtime, which expects React on the global.
beforeEach(() => {
  vi.stubGlobal("React", React);
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  server.commands = [
    { name: "clear", description: "Clear the conversation", argumentHint: "", kind: "command" },
    { name: "compact", description: "Summarise the conversation", argumentHint: "", kind: "command" },
    { name: "context", description: "Show what fills the context", argumentHint: "", kind: "command" },
    { name: "retire-session", description: "End this session", argumentHint: "", kind: "command" },
  ];
  server.listCommands.mockReset();
  server.listCommands.mockImplementation(async ({ agentId }: { agentId: string }) => ({
    agentId,
    commands: server.commands,
    error: null,
    requestId: "r1",
  }));
  server.getDirectorySuggestions.mockReset();
  server.getDirectorySuggestions.mockResolvedValue({ entries: [], directories: [], error: null });
});

interface Mounted {
  root: Root;
  container: HTMLDivElement;
}
const mounted: Mounted[] = [];

afterEach(() => {
  for (const entry of mounted.splice(0)) {
    act(() => entry.root.unmount());
    entry.container.remove();
  }
});

function mountComposer(inputMode: ComposerInputMode) {
  let text = "";
  const listeners = new Set<() => void>();
  const cursor = createStore<number>(() => 0);
  const textSource = {
    getSnapshot: () => text,
    subscribe: (listener: () => void) => {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
  };
  const setText = (next: string) => {
    text = next;
    cursor.setState(next.length, true);
    for (const listener of listeners) listener();
  };
  const handle: RefObject<ComposerAutocompleteHandle | null> = { current: null };
  const configuration = {
    setUserInput: setText,
    serverId: "server-1",
    agentId: "conversation-1",
    canExecuteClientSlashCommand: true,
    onClientSlashCommand: vi.fn(),
  };
  const recall = new ComposerRecall();
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  act(() =>
    root.render(
      <QueryClientProvider client={client}>
        <KeyboardShiftProvider>
          <PortalProvider>
            <ComposerKeyboardScopeProvider isPaneFocused isActiveComposer>
              <View style={{ marginTop: 400, width: 600 }}>
                <ComposerAutocompleteBinding
                  text={textSource}
                  cursor={cursor}
                  recall={recall}
                  inputRef={{ current: null }}
                  anchorStyle={undefined}
                  mode={resolveComposerInputMode(inputMode)}
                  show
                  ref={handle}
                  configuration={configuration}
                >
                  <View style={{ height: 40 }} />
                </ComposerAutocompleteBinding>
              </View>
            </ComposerKeyboardScopeProvider>
            <FloatingPanelPortalHost />
          </PortalProvider>
        </KeyboardShiftProvider>
      </QueryClientProvider>,
    ),
  );
  mounted.push({ root, container });

  const popover = () =>
    container.querySelector<HTMLElement>('[data-testid="composer-autocomplete-popover"]');
  const rows = () => Array.from(popover()?.querySelectorAll<HTMLElement>('[tabindex="0"]') ?? []);
  /** The names the list shows, top to bottom (the first match sits nearest the message box); null
   * when no list is on screen. */
  const listed = () => {
    if (!popover()) return null;
    return rows().map((row) => row.innerText.split("\n")[0]);
  };
  const settle = async () => {
    for (let frame = 0; frame < 5; frame++) {
      await act(async () => {
        await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
      });
    }
  };
  const type = async (next: string) => {
    act(() => setText(next));
    await settle();
  };
  const press = async (key: string) => {
    let handled = false;
    act(() => {
      handled =
        handle.current?.onKeyPress({
          key,
          hasModifier: false,
          preventDefault: () => {},
          input: { text, selection: { start: text.length, end: text.length } },
        }) ?? false;
    });
    await settle();
    return handled;
  };
  return { type, press, listed, rows, popover, text: () => text, configuration };
}

describe("the chat's slash command list", () => {
  it("lists the conversation's commands when the message starts with a slash", async () => {
    const composer = mountComposer("chat");
    await composer.type("/");

    await expect.poll(composer.listed).toEqual(["/retire-session", "/context", "/compact"]);
    // the highlighted command's card, over the list
    expect(composer.popover()?.innerText).toMatch(/^\/compact\nSummarise the conversation\n/u);
    expect(server.listCommands).toHaveBeenCalledWith(
      expect.objectContaining({ agentId: "conversation-1" }),
    );
  });

  it("narrows the list to what is typed, and Enter writes the highlighted name", async () => {
    const composer = mountComposer("chat");
    await composer.type("/ret");
    await expect.poll(composer.listed).toEqual(["/retire-session"]);

    expect(await composer.press("Enter")).toBe(true);
    expect(composer.text()).toBe("/retire-session ");
    await expect.poll(composer.listed).toBeNull();
  });

  it("writes the name of the command that is clicked", async () => {
    const composer = mountComposer("chat");
    await composer.type("/");
    await expect.poll(composer.listed).toContain("/context");

    const row = composer.rows().find((element) => element.innerText.startsWith("/context"));
    expect(row).toBeDefined();
    act(() => row?.click());
    expect(composer.text()).toBe("/context ");
  });

  // The app runs `/clear` itself when it is sent (it ends the conversation): a click in a list is
  // not the place for that, and the agent's command of the same name never gets the message.
  it("leaves out the names the app runs itself", async () => {
    const composer = mountComposer("chat");
    await composer.type("/cl");
    await new Promise((resolve) => setTimeout(resolve, 100));
    await composer.type("/cl");

    expect(server.listCommands).toHaveBeenCalled();
    expect(composer.listed()).toBeNull();
    expect(await composer.press("Enter")).toBe(false);
    expect(composer.configuration.onClientSlashCommand).not.toHaveBeenCalled();
  });

  it("shows no box when there is nothing to list", async () => {
    server.commands = [];
    const composer = mountComposer("chat");
    await composer.type("/");
    await expect.poll(() => server.listCommands.mock.calls.length).toBeGreaterThan(0);
    await composer.type("/");
    expect(composer.popover()).toBeNull();

    // Enter is the message box's again: the message is sent, not swallowed by an empty list.
    expect(await composer.press("Enter")).toBe(false);
  });

  it("does not complete files: the server lists none", async () => {
    const composer = mountComposer("chat");
    await composer.type("see @src");
    await new Promise((resolve) => setTimeout(resolve, 300));
    await composer.type("see @src");

    expect(composer.popover()).toBeNull();
    expect(server.getDirectorySuggestions).not.toHaveBeenCalled();
  });

  it("stays out of a terminal prompt", async () => {
    const composer = mountComposer("terminal");
    await composer.type("/");
    await new Promise((resolve) => setTimeout(resolve, 100));
    await composer.type("/");

    expect(composer.popover()).toBeNull();
    expect(await composer.press("Enter")).toBe(false);
  });
});

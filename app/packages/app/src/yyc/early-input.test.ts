import { readFileSync } from "node:fs";
import path from "node:path";
import { JSDOM } from "jsdom";
import { afterEach, describe, expect, it, vi } from "vitest";
import { adoptEarlyInput, mergeEarlyText } from "./early-input.web";

const indexHtml = readFileSync(path.resolve(__dirname, "../../public/index.html"), "utf8");

interface Early {
  take(path: string): { text: string; selStart: number; selEnd: number; base: string } | null;
  release(reason: string): void;
  ready(handoff: { element: unknown; adopt: (focus?: boolean) => void; submit: () => void }): void;
  state(): { active: boolean; composing: boolean; focused: boolean };
  whenIdle(run: () => void): void;
}

/** public/index.html with its inline scripts run, at `url`. */
function page(url: string, options: { signedOut?: boolean; session?: Record<string, string> } = {}) {
  let html = indexHtml;
  if (options.signedOut) html = html.replace("<head>", '<head><meta name="yyc-signed-out" content="1">');
  const dom = new JSDOM(html, {
    url,
    runScripts: "dangerously",
    pretendToBeVisual: true,
    beforeParse(window) {
      for (const [key, value] of Object.entries(options.session ?? {})) {
        window.sessionStorage.setItem(key, value);
      }
      Object.assign(window, { matchMedia: () => ({ matches: true }) });
    },
  });
  const window = dom.window;
  const early = (window as unknown as { __yycEarly: Early }).__yycEarly;
  const box = () => window.document.getElementById("yyc-early-input") as HTMLTextAreaElement | null;
  const type = (text: string) => {
    const element = box()!;
    element.focus();
    element.value += text;
    element.setSelectionRange(element.value.length, element.value.length);
    element.dispatchEvent(new window.Event("input"));
  };
  return { window, early, box, type };
}

afterEach(() => {
  vi.useRealTimers();
});

describe("public/index.html early message box", () => {
  it("is there to type in on a new conversation, outside the decorative frame", () => {
    const { window, box, early } = page("https://ai.example/new");
    expect(box()).not.toBeNull();
    expect(window.document.querySelector("#boot #yyc-early-input")).toBeNull();
    expect(box()!.getAttribute("aria-label")).toBeTruthy();
    expect(early.state().active).toBe(true);
    expect(window.__yycBootFrame).not.toContain("yyc-early");
  });

  it("is skipped on pages without a message box, signed out, and in view-as tabs", () => {
    expect(page("https://ai.example/settings/general").box()).toBeNull();
    expect(page("https://ai.example/s/abc").box()).toBeNull();
    expect(page("https://ai.example/new", { signedOut: true }).box()).toBeNull();
    expect(page("https://ai.example/new#jarvis-view-as=t.x").box()).toBeNull();
    expect(page("https://ai.example/new", { session: { "yyc.view-as": "t.x" } }).box()).toBeNull();
    expect(page("https://ai.example/h/a/workspace/b/").box()).not.toBeNull();
  });

  it("puts the box at the bottom on a conversation page", () => {
    const { window } = page("https://ai.example/h/a/agent/b");
    expect(window.document.documentElement.classList.contains("yyc-conv")).toBe(true);
  });

  it("hands over the live text with its selection, then clears the copy", () => {
    const { early, type, window } = page("https://ai.example/new");
    type("hello");
    expect(early.take("/new")).toEqual({ text: "hello", selStart: 5, selEnd: 5, base: "" });
    expect(window.sessionStorage.getItem("yyc-early-input")).toBeNull();
    early.release("adopted");
    expect(window.document.getElementById("yyc-early")).toBeNull();
    expect(early.take("/new")).toBeNull();
  });

  it("keeps text from a removed box only for the same page, or /new within a minute", () => {
    const { early, type } = page("https://ai.example/h/a/workspace/b");
    type("draft");
    early.release("no-composer");
    expect(early.take("/h/a/workspace/c")).toBeNull();
    const again = page("https://ai.example/h/a/workspace/b");
    again.type("draft");
    again.early.release("timeout");
    expect(again.early.take("/h/a/workspace/b/")?.text).toBe("draft");
    const stale = JSON.stringify({ text: "old", selStart: 3, selEnd: 3, base: "", path: "/x", t: 0 });
    expect(page("https://ai.example/settings", { session: { "yyc-early-input": stale } }).early.take("/new")).toBeNull();
  });

  it("does not send on Enter before the app is ready, and keeps the text", () => {
    const { window, box, type } = page("https://ai.example/new");
    Object.defineProperty(window, "innerWidth", { value: 1440 });
    type("hi");
    const enter = new window.KeyboardEvent("keydown", { key: "Enter", cancelable: true });
    box()!.dispatchEvent(enter);
    expect(enter.defaultPrevented).toBe(true);
    expect(box()!.value).toBe("hi");
  });

  it("removes the box 1.5 s after the app shows something that is not loading", async () => {
    const { window, early } = page("https://ai.example/new");
    const root = window.document.getElementById("root")!;
    root.innerHTML = "<div>an app page</div>";
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(early.state().active).toBe(true);
    await new Promise((resolve) => setTimeout(resolve, 1_600));
    expect(early.state().active).toBe(false);
  });

  it("stays while the connecting frame or a loading area shows", async () => {
    const { window, early } = page("https://ai.example/new");
    const root = window.document.getElementById("root")!;
    root.innerHTML = '<div data-yyc-loading=""></div>';
    await new Promise((resolve) => setTimeout(resolve, 1_600));
    expect(early.state().active).toBe(true);
  });

  it("on a phone, swaps to the app's box on the next key and sends on Enter after ready", () => {
    const { window, early, box, type } = page("https://ai.example/new");
    Object.defineProperty(window, "innerWidth", { value: 1440 });
    type("hi");
    const adopt = vi.fn(() => early.release("adopted"));
    const submit = vi.fn();
    early.ready({ element: null, adopt, submit });
    box()!.dispatchEvent(new window.KeyboardEvent("keydown", { key: "Enter", cancelable: true }));
    expect(adopt).toHaveBeenCalledTimes(1);
    expect(submit).toHaveBeenCalledTimes(1);
  });

  it("on a phone, a first tap anywhere else hands the text over before that tap acts", () => {
    const { window, early, type } = page("https://ai.example/new");
    type("hello");
    const adopt = vi.fn(() => early.release("adopted"));
    early.ready({ element: null, adopt, submit: vi.fn() });
    const send = window.document.createElement("button");
    window.document.body.appendChild(send);
    let adoptedFirst = false;
    send.addEventListener("pointerdown", () => {
      adoptedFirst = adopt.mock.calls.length === 1;
    });
    send.dispatchEvent(new window.Event("pointerdown", { bubbles: true }));
    expect(adopt).toHaveBeenCalledWith(false);
    expect(adoptedFirst).toBe(true);
    expect(window.document.getElementById("yyc-early")).toBeNull();
  });

  it("on a phone, leaving the box hands the text over without pulling the keyboard back", () => {
    const { window, early, box, type } = page("https://ai.example/new");
    type("hello");
    const adopt = vi.fn();
    early.ready({ element: null, adopt, submit: vi.fn() });
    box()!.dispatchEvent(new window.FocusEvent("blur"));
    expect(adopt).toHaveBeenCalledWith(false);
  });

  it("does not catch taps once it is gone", () => {
    const { window, early } = page("https://ai.example/new");
    const adopt = vi.fn();
    early.ready({ element: null, adopt, submit: vi.fn() });
    early.release("composer-gone");
    window.document.body.dispatchEvent(new window.Event("pointerdown", { bubbles: true }));
    expect(adopt).not.toHaveBeenCalled();
  });
});

describe("mergeEarlyText", () => {
  const taken = { text: "new words", selStart: 3, selEnd: 3, base: "" };

  it("fills an empty draft and keeps the caret", () => {
    expect(mergeEarlyText("", taken)).toEqual({ text: "new words", selection: { start: 3, end: 3 } });
  });

  it("never overwrites a stored draft: appends on a new line, caret at the end", () => {
    expect(mergeEarlyText("kept", taken)).toEqual({
      text: "kept\nnew words",
      selection: { start: 14, end: 14 },
    });
  });

  it("adopts nothing from an untouched box", () => {
    expect(mergeEarlyText("kept", { text: "", selStart: 0, selEnd: 0, base: "" })).toBeNull();
  });
});

describe("adoptEarlyInput", () => {
  function fakeEarly(state: { active: boolean; composing: boolean; focused: boolean }) {
    let idle: (() => void) | null = null;
    const api = {
      take: vi.fn(() => ({ text: "typed", selStart: 5, selEnd: 5, base: "" })),
      release: vi.fn(),
      ready: vi.fn(),
      state: () => state,
      whenIdle: (run: () => void) => {
        if (state.composing) idle = run;
        else run();
      },
    };
    vi.stubGlobal("window", {
      __yycEarly: api,
      location: { pathname: "/new" },
      matchMedia: () => ({ matches: true }),
    });
    return { api, finishComposition: () => idle?.() };
  }

  function target(stored = "") {
    return {
      element: null,
      getText: () => stored,
      replaceText: vi.fn(),
      focus: vi.fn(),
      submit: vi.fn(),
    };
  }

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("on desktop takes the text, focuses and removes the early box in one go", () => {
    const { api } = fakeEarly({ active: true, composing: false, focused: true });
    const input = target();
    adoptEarlyInput(input);
    expect(input.replaceText).toHaveBeenCalledWith("typed", { start: 5, end: 5 });
    expect(input.focus).toHaveBeenCalled();
    expect(api.release).toHaveBeenCalledWith("adopted");
  });

  it("waits for an IME composition to end", () => {
    const { api, finishComposition } = fakeEarly({ active: true, composing: true, focused: true });
    const input = target();
    adoptEarlyInput(input);
    expect(api.take).not.toHaveBeenCalled();
    finishComposition();
    expect(input.replaceText).toHaveBeenCalled();
  });

  it("on a phone with the keyboard up, hands over at the next tap instead", () => {
    const { api } = fakeEarly({ active: true, composing: false, focused: true });
    (window as unknown as { matchMedia: () => { matches: boolean } }).matchMedia = () => ({ matches: false });
    const input = target();
    adoptEarlyInput(input);
    expect(api.ready).toHaveBeenCalledTimes(1);
    expect(input.replaceText).not.toHaveBeenCalled();
  });

  it("on a phone, a handover without focus does not focus, and an unmount drops the box", () => {
    const { api } = fakeEarly({ active: true, composing: false, focused: true });
    (window as unknown as { matchMedia: () => { matches: boolean } }).matchMedia = () => ({ matches: false });
    const input = target();
    const cleanup = adoptEarlyInput(input);
    const handoff = (api.ready.mock.calls[0] as unknown as [{ adopt: (focus?: boolean) => void }])[0];
    handoff.adopt(false);
    expect(input.replaceText).toHaveBeenCalledWith("typed", { start: 5, end: 5 });
    expect(input.focus).not.toHaveBeenCalled();
    api.release.mockClear();
    cleanup?.();
    expect(api.release).toHaveBeenCalledWith("composer-gone");
  });
});

describe("adoptEarlyInput after the box is gone", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("still takes text kept for this page", () => {
    const take = vi.fn(() => ({ text: "kept", selStart: 4, selEnd: 4, base: "" }));
    vi.stubGlobal("window", {
      __yycEarly: { take, release: vi.fn(), ready: vi.fn(), state: () => ({ active: false }), whenIdle: vi.fn() },
      location: { pathname: "/new" },
      matchMedia: () => ({ matches: true }),
    });
    const replaceText = vi.fn();
    adoptEarlyInput({ element: null, getText: () => "", replaceText, focus: vi.fn(), submit: vi.fn() });
    expect(replaceText).toHaveBeenCalledWith("kept", { start: 4, end: 4 });
  });
});

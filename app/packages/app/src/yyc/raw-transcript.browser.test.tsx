import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { RawTranscriptChip } from "./raw-transcript";
import { parseViewAsToken, useViewAsStore } from "./view-as";

// Deyao, 2026-11-19: pigeon's fixed "Raw transcript" button covered the header's chips. The admin's raw transcript is
// now a chip in the header itself, shown only in a view-as tab on a conversation.

const b64url = (value: string) => btoa(value).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
const token = `${b64url(JSON.stringify({ a: "admin@yyc.hk", u: "yoyo.ma@yyc.hk", e: 4_102_444_800 }))}.c2ln`;

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("React", React);
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  useViewAsStore.setState({ viewAs: null, override: false });
  vi.unstubAllGlobals();
});

const chip = () => document.querySelector<HTMLElement>('[data-testid="raw-transcript-chip"]');

it("shows in a view-as tab on a conversation, as a link", () => {
  useViewAsStore.setState({ viewAs: parseViewAsToken(token) });
  act(() => root.render(<RawTranscriptChip sessionId="0e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b" />));
  expect(chip()).not.toBeNull();
  expect(chip()?.getAttribute("role")).toBe("link");
});

it("shows nothing in a normal tab", () => {
  act(() => root.render(<RawTranscriptChip sessionId="0e1f2a3b-4c5d-4e6f-8a9b-0c1d2e3f4a5b" />));
  expect(chip()).toBeNull();
});

it("shows nothing on a new chat (no conversation yet)", () => {
  useViewAsStore.setState({ viewAs: parseViewAsToken(token) });
  act(() => root.render(<RawTranscriptChip sessionId={null} />));
  expect(chip()).toBeNull();
});

import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Text } from "react-native";
import { page, userEvent } from "vitest/browser";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import "@/i18n/i18next";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";

// Deyao, 2026-11-18: the model menu opened with its first row painted as hovered while the pointer was elsewhere.
// Opening focused the first row, and a focused row is painted as the hovered one. A menu opened with the pointer
// shows no row as hovered until the pointer is over one; the arrow keys still reach the rows.

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("React", React);
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
  focusTrail.length = 0;
  document.addEventListener("focusin", onFocusIn, true);
});
afterEach(() => {
  document.removeEventListener("focusin", onFocusIn, true);
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

const rows = () => Array.from(document.querySelectorAll<HTMLElement>('[data-menu-item="true"]'));
const surface = () => document.querySelector<HTMLElement>('[data-menu-surface="true"]');
// The surface is drawn off-screen (top -9999) to be measured, then mounted again in its place: wait for that one.
const placed = () => {
  const el = surface();
  return el != null && el.getBoundingClientRect().top > -1000;
};
const fill = (row: HTMLElement) => getComputedStyle(row).backgroundColor;
const frames = () => new Promise<void>((done) => requestAnimationFrame(() => requestAnimationFrame(() => done())));
// What focus did, for a failure's message (CI's job logs need a login; its annotations show this text)
const focusTrail: string[] = [];
const named = (el: Element | null) =>
  !el
    ? "nothing"
    : el === document.body
      ? "body"
      : [el.getAttribute("data-testid"), el.getAttribute("role"), el.hasAttribute("data-menu-surface") ? "surface" : null]
          .filter(Boolean)
          .join("/") || el.tagName.toLowerCase();
const onFocusIn = (event: FocusEvent) =>
  focusTrail.push(`${named(event.target as Element)}${placed() ? "" : " (menu not placed)"}`);
async function until(check: () => boolean, what: string): Promise<void> {
  // a loaded CI runner (unit and browser projects at once) takes seconds to place a menu
  for (let i = 0; i < 500; i += 1) {
    if (check()) return;
    await new Promise((done) => setTimeout(done, 20));
  }
  throw new Error(
    `timed out waiting for ${what}: focus on ${named(document.activeElement)}, menu ${placed() ? "placed" : "not placed"}, focus went to: ${focusTrail.join(" → ") || "nothing"}`,
  );
}

it("opens with no row painted as hovered, and the arrow keys still reach the rows", async () => {
  act(() =>
    root.render(
      <DropdownMenu>
        <DropdownMenuTrigger testID="open" accessibilityRole="button">
          <Text>Open</Text>
        </DropdownMenuTrigger>
        <DropdownMenuContent testID="menu" side="bottom" align="start">
          <DropdownMenuItem testID="row-a">A</DropdownMenuItem>
          <DropdownMenuItem testID="row-b">B</DropdownMenuItem>
          <DropdownMenuItem testID="row-c">C</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>,
    ),
  );
  await userEvent.click(page.getByTestId("open"));
  // the menu is in its place and its open has moved focus onto it (not onto the measuring copy, which goes)
  await until(placed, "the menu in its place");
  await until(() => document.activeElement === surface(), "focus on the placed menu");
  await frames();
  expect(placed()).toBe(true);
  const [a, b, c] = rows();
  expect(document.activeElement?.getAttribute("data-menu-item")).toBeNull();
  expect(fill(a)).toBe(fill(c));
  expect(fill(b)).toBe(fill(c));

  await userEvent.keyboard("{ArrowDown}");
  expect(named(document.activeElement), focusTrail.join(" → ")).toBe(named(a));
  await until(() => fill(a) !== fill(c), "the focused row painted");
  await userEvent.keyboard("{ArrowDown}");
  expect(named(document.activeElement), focusTrail.join(" → ")).toBe(named(b));
}, 30_000);

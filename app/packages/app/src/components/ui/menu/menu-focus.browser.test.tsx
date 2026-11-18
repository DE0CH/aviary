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
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

const rows = () => Array.from(document.querySelectorAll<HTMLElement>('[data-menu-item="true"]'));
const fill = (row: HTMLElement) => getComputedStyle(row).backgroundColor;
const frames = () => new Promise<void>((done) => requestAnimationFrame(() => requestAnimationFrame(() => done())));
async function until(check: () => boolean): Promise<void> {
  for (let i = 0; i < 150; i += 1) {
    if (check()) return;
    await new Promise((done) => setTimeout(done, 20));
  }
  throw new Error("timed out");
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
  // the menu is placed and its open has moved focus into it
  await until(() => document.activeElement?.closest('[data-menu-surface="true"]') != null);
  await frames();
  const [a, b, c] = rows();
  expect(document.activeElement?.getAttribute("data-menu-item")).toBeNull();
  expect(fill(a)).toBe(fill(c));
  expect(fill(b)).toBe(fill(c));

  await userEvent.keyboard("{ArrowDown}");
  expect(document.activeElement).toBe(a);
  await until(() => fill(a) !== fill(c));
  await userEvent.keyboard("{ArrowDown}");
  expect(document.activeElement).toBe(b);
});

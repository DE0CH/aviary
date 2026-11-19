import React, { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { View } from "react-native";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { i18n } from "@/i18n/i18next";
import { LevelLabelProbe, ThinkingLevels } from "./model-menu-levels";
import { LEVEL_LABEL_ROOM, MENU_MIN_WIDTH, menuWidth } from "./model-menu-state";

// Deyao, 2026-11-18: the thinking row of six levels (Claude's five efforts and none) clipped its English labels
// (Medi…, Stan…, Exte…) and was cramped. The menu's width now follows the widest label as it renders, so every cell
// holds its label with a menu row's room each side, in whatever font the browser has.

const LEVELS = ["none", "low", "medium", "high", "xhigh", "max"];
const label = (level: string) => i18n.t(`conversation.model.level.${level}`);

let root: Root;
let container: HTMLDivElement;
beforeEach(() => {
  vi.stubGlobal("React", React);
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

let shownWidth = 0;
function Menu() {
  const [widest, setWidest] = useState<number | null>(null);
  const labels = LEVELS.map(label);
  shownWidth = menuWidth(widest, LEVELS.length, 1280);
  return (
    <View>
      <LevelLabelProbe labels={labels} onWidest={setWidest} />
      {widest === null ? null : (
        <View style={{ width: shownWidth }}>
          <ThinkingLevels row={LEVELS} offered={LEVELS} picked="high" label={label} onPick={() => {}} />
        </View>
      )}
    </View>
  );
}

async function cells(): Promise<HTMLElement[]> {
  for (let i = 0; i < 150; i += 1) {
    const found = Array.from(container.querySelectorAll<HTMLElement>('[data-testid^="yyc-model-level-"]')).filter(
      (el) => el.dataset.testid !== "yyc-model-level-probe",
    );
    if (found.length === LEVELS.length) return found;
    await new Promise((done) => setTimeout(done, 20));
  }
  throw new Error("the row never rendered");
}

function room(cell: HTMLElement): { clipped: boolean; each: number; text: string } {
  const text = cell.firstElementChild as HTMLElement;
  const inner = cell.getBoundingClientRect().width - 2; // the cell's 1 px border each side
  return {
    clipped: text.scrollWidth > text.clientWidth,
    each: (inner - text.getBoundingClientRect().width) / 2,
    text: text.textContent ?? "",
  };
}

async function expectRoomy() {
  for (const cell of await cells()) {
    const r = room(cell);
    expect(r.clipped, `${r.text} is clipped`).toBe(false);
    expect(r.each, `${r.text} has ${r.each.toFixed(1)} px each side`).toBeGreaterThanOrEqual(LEVEL_LABEL_ROOM - 0.5);
  }
}

it("holds every label of a six-level row with a menu row's room each side, and is only as wide as its labels need", async () => {
  act(() => root.render(<Menu />));
  await expectRoomy();
  const english = shownWidth;
  expect(english).toBeGreaterThan(MENU_MIN_WIDTH);
  act(() => root.unmount());
  root = createRoot(container);

  // zh-HK's labels are two characters: the same room, a narrower menu
  await act(async () => {
    await i18n.changeLanguage("zh-HK");
  });
  act(() => root.render(<Menu />));
  await expectRoomy();
  expect(shownWidth).toBeLessThan(english);
});

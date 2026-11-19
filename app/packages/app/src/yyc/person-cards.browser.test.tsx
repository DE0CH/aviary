import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { i18n } from "@/i18n/i18next";
import { PersonCardView } from "./person-cards";
import type { PersonCard } from "./person-cards-text";

// Deyao, 2026-11-18 (the approved three-state mock): a question or a wait on the person is one card that waits where
// it was asked, says it is answered but not read yet, and once read shows where the AI read it.

let root: Root;
let container: HTMLDivElement;
beforeEach(async () => {
  vi.stubGlobal("React", React);
  await i18n.changeLanguage("en");
  container = document.createElement("div");
  document.body.appendChild(container);
  root = createRoot(container);
});
afterEach(() => {
  act(() => root.unmount());
  container.remove();
  vi.unstubAllGlobals();
});

const question: PersonCard = {
  kind: "question",
  id: "q-1",
  input: { questions: [{ question: "Which day?", options: [{ label: "Tue" }, { label: "Thu" }] }] },
  marker: "(answer to q-1)",
  state: "open",
  askedAt: "2026-11-18T10:01:00Z",
};
const answered: PersonCard = {
  ...question,
  state: "answered",
  answeredAt: "2026-11-18T14:15:00Z",
  answer: { behavior: "allow", updatedInput: { answers: { "Which day?": "Thu" } } },
};
const shown = () => container.textContent ?? "";

it("waits where it was asked, with the options left to the answer card", () => {
  act(() => root.render(<PersonCardView card={question} view="waiting" />));
  expect(container.querySelector('[data-testid="person-card-waiting"]')).not.toBeNull();
  expect(shown()).toContain("Waiting for you");
  expect(shown()).toContain("Which day?");
  expect(shown()).not.toContain("Thu");
  expect(shown()).toContain("Answer it in the card at the bottom.");
});

it("shows the answer, not read yet", () => {
  act(() => root.render(<PersonCardView card={answered} view="unread" />));
  expect(shown()).toContain("Not read yet");
  expect(shown()).toContain("Thu");
  expect(shown()).toContain("The AI hasn't read it yet.");
});

it("once read where it moved to, says when it was asked", () => {
  act(() => root.render(<PersonCardView card={{ ...answered, readBy: "toolu_cat" }} view="read" />));
  expect(shown()).toMatch(/Question for you, asked \d+:01/);
  expect(shown()).toMatch(/Answered \d+:15/);
  expect(shown()).not.toContain("Not read yet");
});

it("shows a wait's label, title, reason and outcome, in Chinese too", async () => {
  const wait: PersonCard = {
    ...question,
    kind: "wait",
    id: "w-1",
    input: { label: "Inbox request", title: "Deploy key for DE0CH/aviary", why: "Push it." },
    state: "approved",
    answeredAt: "2026-11-18T10:02:00Z",
  };
  act(() => root.render(<PersonCardView card={wait} view="read" />));
  expect(shown()).toContain("Inbox request");
  expect(shown()).toContain("Deploy key for DE0CH/aviary");
  expect(shown()).toContain("You approved it");
  await act(async () => {
    await i18n.changeLanguage("zh-HK");
  });
  act(() => root.render(<PersonCardView card={{ ...wait, state: "open" }} view="waiting" />));
  expect(shown()).toContain("等你回覆");
});

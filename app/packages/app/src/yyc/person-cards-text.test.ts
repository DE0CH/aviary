import { describe, expect, it } from "vitest";
import {
  cardAnswers,
  cardQuestions,
  cardViewWhereAsked,
  isPersonCardCall,
  moved,
  readPersonAsk,
  readPersonRead,
  stateWord,
  waitText,
  type PersonCard,
} from "./person-cards-text";

// A question the AI asked with `ask`, or a wait on the person (aviary magpie rule 11, Deyao 2026-11-18): one card in
// three states, where it was asked until the AI reads the answer, then on the call that read it.
const question: PersonCard = {
  kind: "question",
  id: "q-1",
  input: {
    questions: [
      { question: "Which day?", header: "Day", options: [{ label: "Tue" }, { label: "Thu", description: "26 Nov" }] },
      { question: "Which redirects?", multiSelect: true, options: [{ label: "repo" }, { label: "image" }] },
    ],
  },
  marker: "(answer to q-1)",
  state: "open",
  askedAt: "2026-11-18T10:01:00Z",
};

describe("where a card shows", () => {
  it("waits where it was asked, stays there unread once answered, and leaves once read", () => {
    expect(cardViewWhereAsked(question)).toBe("waiting");
    expect(cardViewWhereAsked({ ...question, state: "answered" })).toBe("unread");
    expect(cardViewWhereAsked({ ...question, state: "approved", kind: "wait" })).toBe("unread");
    expect(cardViewWhereAsked({ ...question, state: "answered", readBy: "toolu_cat" })).toBeNull();
  });

  it("reads the card off the call where it was asked and the cards off the call that read them", () => {
    expect(readPersonAsk({ personAsk: question })?.id).toBe("q-1");
    expect(readPersonAsk({ description: "x" })).toBeNull();
    expect(readPersonAsk({ personAsk: { id: 3 } })).toBeNull();
    expect(readPersonRead({ personRead: [question, { nope: true }] }).map((c) => c.id)).toEqual(["q-1"]);
    expect(readPersonRead(undefined)).toEqual([]);
    expect(isPersonCardCall({ personRead: [question] })).toBe(true);
    expect(isPersonCardCall({ personAsk: question })).toBe(true);
    expect(isPersonCardCall({ background: true })).toBe(false);
  });
});

describe("what a card says", () => {
  it("reads each question's options and the person's picks, typed answer and note", () => {
    expect(cardQuestions(question).map((q) => [q.question, q.multiSelect, q.options.length])).toEqual([
      ["Which day?", false, 2],
      ["Which redirects?", true, 2],
    ]);
    const answered = {
      ...question,
      state: "answered",
      answer: {
        behavior: "allow",
        updatedInput: {
          answers: { "Which day?": "Thu", "Which redirects?": "repo, the clone URLs" },
          annotations: { "Which day?": { notes: " the 09:12 train " } },
        },
      },
    };
    const a = cardAnswers(answered);
    expect(a.get("Which day?")).toEqual({ picked: ["Thu"], other: null, notes: "the 09:12 train" });
    expect(a.get("Which redirects?")).toEqual({ picked: ["repo"], other: "the clone URLs", notes: null });
    expect(cardAnswers(question).size).toBe(0);
  });

  it("names the state in a word, an unknown outcome as written", () => {
    expect(stateWord({ ...question, state: "answered" }, "en")).toBe("Answered");
    expect(stateWord({ ...question, state: "approved" }, "zh")).toBe("已批准");
    expect(stateWord({ ...question, state: "postponed" }, "en")).toBe("postponed");
  });

  it("reads a wait's label, title and reason", () => {
    const wait = { ...question, kind: "wait", input: { label: "Inbox request", title: " Add a store ", why: "Mail" } };
    expect(waitText(wait)).toEqual({ label: "Inbox request", title: "Add a store", why: "Mail" });
    expect(waitText({ ...wait, input: { title: "x" } }).label).toBeNull();
  });

  it("says when it was asked only when it moved a while after", () => {
    expect(moved({ ...question, answeredAt: "2026-11-18T10:03:00Z" })).toBe(false);
    expect(moved({ ...question, answeredAt: "2026-11-18T14:15:00Z" })).toBe(true);
    expect(moved(question)).toBe(false);
  });
});

import { describe, expect, test } from "vitest";
import {
  areQuestionsAnswered,
  buildQuestionFormAnswers,
  OTHER_OPTION_INDEX,
  parseQuestionFormQuestions,
  questionShowsTextInput,
  resolveDismissLabel,
  shouldSubmitEmptyOnDismiss,
} from "./question-form-card-core";

describe("question form card core", () => {
  test("treats optional input prompts as skippable empty answers", () => {
    const questions = parseQuestionFormQuestions({
      questions: [
        {
          question: "Optional comment?",
          header: "Response",
          options: [],
          multiSelect: false,
          placeholder: "Optional comment (press Enter to skip)...",
          allowEmpty: true,
          dismissLabel: "Skip",
        },
      ],
    });

    if (!questions) throw new Error("questions did not parse");
    expect(areQuestionsAnswered(questions, {}, {})).toBe(true);
    expect(buildQuestionFormAnswers(questions, {}, {})).toEqual({ Response: "" });
    expect(shouldSubmitEmptyOnDismiss(questions)).toBe(true);
    expect(resolveDismissLabel(questions)).toBe("Skip");
  });

  test("requires a selection for option-only questions", () => {
    const questions = parseQuestionFormQuestions({
      questions: [
        {
          question: "Pick one",
          header: "Response",
          options: [{ label: "A" }, { label: "B" }],
          multiSelect: false,
        },
      ],
    });

    if (!questions) throw new Error("questions did not parse");
    const [question] = questions;
    if (!question) throw new Error("question missing");
    expect(questionShowsTextInput(question)).toBe(false);
    expect(areQuestionsAnswered(questions, {}, { 0: "freeform" })).toBe(false);
    expect(areQuestionsAnswered(questions, { 0: new Set([1]) }, {})).toBe(true);
    expect(buildQuestionFormAnswers(questions, { 0: new Set([1]) }, {})).toEqual({
      Response: "B",
    });
  });

  test("keeps checked options and appends the other answer for multi-select", () => {
    const questions = parseQuestionFormQuestions({
      questions: [
        {
          question: "Which fruits do you like?",
          header: "Fruits",
          options: [{ label: "Apple" }, { label: "Banana" }, { label: "Cherry" }],
          multiSelect: true,
          allowOther: true,
        },
      ],
    });

    if (!questions) throw new Error("questions did not parse");
    const other = OTHER_OPTION_INDEX;
    expect(
      buildQuestionFormAnswers(questions, { 0: new Set([0, 2, other]) }, { 0: " durian " }),
    ).toEqual({ Fruits: "Apple, Cherry, durian" });
    expect(buildQuestionFormAnswers(questions, { 0: new Set([0, 2]) }, {})).toEqual({
      Fruits: "Apple, Cherry",
    });
    expect(buildQuestionFormAnswers(questions, { 0: new Set([other]) }, { 0: "durian" })).toEqual({
      Fruits: "durian",
    });
    // text in the box of an unchecked "Other" is not an answer
    expect(buildQuestionFormAnswers(questions, { 0: new Set([0]) }, { 0: "durian" })).toEqual({
      Fruits: "Apple",
    });
    expect(areQuestionsAnswered(questions, {}, { 0: "durian" })).toBe(false);
    // a checked "Other" with an empty box is not one either
    expect(areQuestionsAnswered(questions, { 0: new Set([other]) }, {})).toBe(false);
    expect(areQuestionsAnswered(questions, { 0: new Set([other]) }, { 0: "durian" })).toBe(true);
  });

  test("replaces the selected option with the other answer for single-select", () => {
    const questions = parseQuestionFormQuestions({
      questions: [
        {
          question: "Which provider?",
          header: "Provider",
          options: [{ label: "Claude Code" }, { label: "Codex" }],
          multiSelect: false,
          allowOther: true,
        },
      ],
    });

    if (!questions) throw new Error("questions did not parse");
    expect(
      buildQuestionFormAnswers(questions, { 0: new Set([OTHER_OPTION_INDEX]) }, { 0: "OpenCode" }),
    ).toEqual({ Provider: "OpenCode" });
    // the option's radio is the one picked: the text left in the "Other" box is not sent
    expect(buildQuestionFormAnswers(questions, { 0: new Set([1]) }, { 0: "OpenCode" })).toEqual({
      Provider: "Codex",
    });
    expect(buildQuestionFormAnswers(questions, { 0: new Set([1]) }, {})).toEqual({
      Provider: "Codex",
    });
  });

  test("shows text input for explicit other questions", () => {
    const questions = parseQuestionFormQuestions({
      questions: [
        {
          question: "Pick or type",
          header: "Response",
          options: [{ label: "A" }],
          isOther: true,
          multiSelect: false,
        },
      ],
    });

    if (!questions) throw new Error("questions did not parse");
    const [question] = questions;
    if (!question) throw new Error("question missing");
    expect(questionShowsTextInput(question)).toBe(true);
    const picked = { 0: new Set([OTHER_OPTION_INDEX]) };
    expect(areQuestionsAnswered(questions, picked, { 0: "custom" })).toBe(true);
    expect(buildQuestionFormAnswers(questions, picked, { 0: "custom" })).toEqual({
      Response: "custom",
    });
  });

  test("shows text input for questions that allow other answers", () => {
    const questions = parseQuestionFormQuestions({
      questions: [
        {
          question: "Pick or type",
          header: "Response",
          options: [{ label: "A" }],
          allowOther: true,
          multiSelect: false,
        },
      ],
    });

    if (!questions) throw new Error("questions did not parse");
    const [question] = questions;
    if (!question) throw new Error("question missing");
    expect(questionShowsTextInput(question)).toBe(true);
    const picked = { 0: new Set([OTHER_OPTION_INDEX]) };
    expect(areQuestionsAnswered(questions, picked, { 0: "custom" })).toBe(true);
    expect(buildQuestionFormAnswers(questions, picked, { 0: "custom" })).toEqual({
      Response: "custom",
    });
  });

  test("keeps option previews so options can be compared", () => {
    const questions = parseQuestionFormQuestions({
      questions: [
        {
          question: "Which layout?",
          header: "Layout",
          options: [
            {
              label: "Tabs",
              description: "One tab per section",
              preview: "<Tabs>\n  <Tab />\n</Tabs>",
            },
            { label: "Single page" },
          ],
          multiSelect: false,
        },
      ],
    });

    expect(questions?.[0]?.options).toEqual([
      { label: "Tabs", description: "One tab per section", preview: "<Tabs>\n  <Tab />\n</Tabs>" },
      { label: "Single page", description: undefined, preview: undefined },
    ]);
  });
});

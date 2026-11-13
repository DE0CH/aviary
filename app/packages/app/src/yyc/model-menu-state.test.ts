import { describe, expect, it } from "vitest";
import {
  levelForModel,
  menuDefaults,
  menuModels,
  menuState,
  movesHarness,
  pickSettled,
  rememberLevel,
  shownChoice,
} from "./model-menu-state";

const option = (id: string, isDefault = false) => ({ id, label: id, isDefault });
const MODELS = menuModels([
  {
    provider: "claude",
    id: "deepseek",
    label: "DeepSeek V4.1 Flash",
    isDefault: true,
    thinkingOptions: [option("none"), option("low"), option("high", true), option("max")],
    defaultThinkingOptionId: "high",
  },
  {
    provider: "claude",
    id: "glm",
    label: "GLM 5.3",
    thinkingOptions: [option("max"), option("low"), option("high", true)],
  },
  { provider: "claude", id: "plain", label: "" },
]);

describe("the model menu", () => {
  it("reads models with their levels in order and the level a conversation starts on", () => {
    expect(MODELS).toEqual([
      { id: "deepseek", label: "DeepSeek V4.1 Flash", levels: ["none", "low", "high", "max"], defaultLevel: "high", isDefault: true, harness: "claude" },
      { id: "glm", label: "GLM 5.3", levels: ["low", "high", "max"], defaultLevel: "high", isDefault: false, harness: "claude" },
      { id: "plain", label: "plain", levels: [], defaultLevel: null, isDefault: false, harness: "claude" },
    ]);
  });

  it("starts a new conversation on the default model at its own level", () => {
    expect(menuDefaults(MODELS)).toEqual({ modelId: "deepseek", level: "high" });
    expect(menuState(MODELS, null, null).model?.id).toBe("deepseek");
    expect(menuState(MODELS, "gone", "max").model?.id).toBe("deepseek");
  });

  it("keeps the thinking row the same for every model, so nothing moves when the model changes", () => {
    const row = ["none", "low", "high", "max"];
    expect(menuState(MODELS, "deepseek", "max")).toMatchObject({ row, level: "max" });
    // GLM 5.3 cannot switch thinking off: the position stays, the level in effect is its own
    expect(menuState(MODELS, "glm", "none")).toMatchObject({ row, level: "high" });
    expect(menuState(MODELS, "glm", "low")).toMatchObject({ row, level: "low" });
    // a model with no levels: the row stays, nothing is in effect
    expect(menuState(MODELS, "plain", "high")).toMatchObject({ row, level: null });
  });

  it("has no row when no model has levels", () => {
    const plain = menuModels([{ provider: "claude", id: "a", label: "A", isDefault: true }]);
    expect(menuState(plain, null, null)).toMatchObject({ row: [], level: null });
    expect(menuState([], null, null)).toEqual({ models: [], model: null, row: [], level: null });
  });

  // Deyao, 2026-11-13: a click on a thinking level lagged (it showed only once the server had answered).
  it("shows a pick at once and lets it go when the server has it, or has refused it", () => {
    const server = { modelId: "a", level: "low" };
    expect(shownChoice(server, null)).toEqual(server);
    // picked: shown at once, before the server says anything
    expect(shownChoice(server, { level: "high" })).toEqual({ modelId: "a", level: "high" });
    expect(shownChoice(server, { modelId: "b", level: "max" })).toEqual({ modelId: "b", level: "max" });
    // settled once the server's own state is the pick
    expect(pickSettled(server, { level: "high" })).toBe(false);
    expect(pickSettled({ modelId: "a", level: "high" }, { level: "high" })).toBe(true);
    expect(pickSettled({ modelId: "b", level: "low" }, { modelId: "b", level: "max" })).toBe(false);
    expect(pickSettled({ modelId: "b", level: "max" }, { modelId: "b", level: "max" })).toBe(true);
  });

  // Deyao, 2026-11-13: a thinking level per model. Each model keeps the level last picked for it.
  it("gives each model its own remembered level, else its own default", () => {
    const models = menuModels([
      { id: "a", label: "A", thinkingOptions: [{ id: "low" }, { id: "high" }], defaultThinkingOptionId: "low" },
      { id: "b", label: "B", thinkingOptions: [{ id: "high" }, { id: "max" }], defaultThinkingOptionId: "high" },
      { id: "c", label: "C" },
    ] as never);
    const [a, b, c] = models;
    expect(levelForModel(a, {})).toBe("low");
    expect(levelForModel(a, { a: "high", b: "max" })).toBe("high");
    expect(levelForModel(b, { a: "high", b: "max" })).toBe("max");
    // a remembered level the model no longer has: its own default
    expect(levelForModel(b, { b: "low" })).toBe("high");
    expect(levelForModel(c, { c: "high" })).toBeNull();
    expect(rememberLevel({ a: "high" }, "b", "max")).toEqual({ a: "high", b: "max" });
  });
});

describe("a model of another harness", () => {
  const models = menuModels([
    { provider: "claude", id: "deepseek", label: "DeepSeek", isDefault: true },
    { provider: "claude", id: "glm", label: "GLM" },
    { provider: "claude", id: "mimo", label: "MiMo", metadata: { harness: "qwen" } },
  ]);

  it("names its harness; the others are Claude Code's", () => {
    expect(models.map((model) => model.harness)).toEqual(["claude", "claude", "qwen"]);
  });

  it("moves the conversation only when the harness changes", () => {
    expect(movesHarness(models, "deepseek", "glm")).toBe(false);
    expect(movesHarness(models, "deepseek", "mimo")).toBe(true);
    expect(movesHarness(models, "mimo", "glm")).toBe(true);
    expect(movesHarness(models, "mimo", "mimo")).toBe(false);
    // no model picked yet: the conversation is on the default
    expect(movesHarness(models, null, "mimo")).toBe(true);
    expect(movesHarness(models, null, "glm")).toBe(false);
    expect(movesHarness(models, "deepseek", "gone")).toBe(false);
  });
});

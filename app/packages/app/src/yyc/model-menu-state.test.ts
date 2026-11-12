import { describe, expect, it } from "vitest";
import { menuDefaults, menuModels, menuState } from "./model-menu-state";

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
      { id: "deepseek", label: "DeepSeek V4.1 Flash", levels: ["none", "low", "high", "max"], defaultLevel: "high", isDefault: true },
      { id: "glm", label: "GLM 5.3", levels: ["low", "high", "max"], defaultLevel: "high", isDefault: false },
      { id: "plain", label: "plain", levels: [], defaultLevel: null, isDefault: false },
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
});

import type { AgentModelDefinition } from "@getpaseo/protocol/agent-types";

/** The thinking levels in order, lowest first: `none` is thinking off, the rest are effort levels. */
export const LEVEL_ORDER = ["none", "low", "medium", "high", "xhigh", "max"] as const;

export interface MenuModel {
  id: string;
  label: string;
  /** The levels this model has, in LEVEL_ORDER. */
  levels: string[];
  /** The level a conversation starts on. */
  defaultLevel: string | null;
  isDefault: boolean;
}

export interface MenuState {
  models: MenuModel[];
  /** The chosen model: the one named, else the default, else the first. */
  model: MenuModel | null;
  /**
   * The positions of the thinking row: every level any model of the list has. The row is the same for every model,
   * so choosing a model never moves a level; a level the chosen model lacks is disabled in place.
   */
  row: string[];
  /** The level in effect: the one named if the model has it, else the model's own. */
  level: string | null;
}

function rank(level: string): number {
  const index = (LEVEL_ORDER as readonly string[]).indexOf(level);
  return index === -1 ? LEVEL_ORDER.length : index;
}

export function menuModels(models: readonly AgentModelDefinition[] | null | undefined): MenuModel[] {
  return (models ?? []).map((model) => {
    const levels = (model.thinkingOptions ?? []).map((option) => option.id);
    levels.sort((a, b) => rank(a) - rank(b));
    const own =
      model.defaultThinkingOptionId ?? model.thinkingOptions?.find((option) => option.isDefault)?.id ?? null;
    return {
      id: model.id,
      label: model.label || model.id,
      levels,
      defaultLevel: own !== null && levels.includes(own) ? own : null,
      isDefault: model.isDefault === true,
    };
  });
}

export function menuState(
  models: MenuModel[],
  selectedModelId: string | null | undefined,
  selectedLevel: string | null | undefined,
): MenuState {
  const model =
    models.find((entry) => entry.id === selectedModelId) ?? models.find((entry) => entry.isDefault) ?? models[0] ?? null;
  const row = [...new Set(models.flatMap((entry) => entry.levels))].sort((a, b) => rank(a) - rank(b));
  let level: string | null = null;
  if (model && model.levels.length > 0) {
    level = selectedLevel && model.levels.includes(selectedLevel) ? selectedLevel : model.defaultLevel;
  }
  return { models, model, row, level };
}

/** What a new conversation starts on: the default model at its own level. */
export function menuDefaults(models: MenuModel[]): { modelId: string | null; level: string | null } {
  const state = menuState(models, null, null);
  return { modelId: state.model?.id ?? null, level: state.level };
}

/** A conversation's model and level as the server has them. */
export interface ServerChoice {
  modelId: string | null | undefined;
  level: string | null | undefined;
}

/** A pick the person has just made that the server has not confirmed yet. */
export interface PendingPick {
  modelId?: string;
  level?: string | null;
}

/**
 * What the menu shows: the person's pick the moment they make it, the server's state otherwise. A pick is sent to the
 * server, which answers a moment later; showing only the server's state made a click look dead for that moment.
 */
export function shownChoice(server: ServerChoice, pending: PendingPick | null): ServerChoice {
  if (!pending) return server;
  return {
    modelId: pending.modelId ?? server.modelId,
    level: pending.level !== undefined ? pending.level : server.level,
  };
}

/** Whether the server's state has caught up with the pick (it is then let go). */
export function pickSettled(server: ServerChoice, pending: PendingPick): boolean {
  const modelDone = pending.modelId === undefined || pending.modelId === server.modelId;
  const levelDone = pending.level === undefined || (pending.level ?? null) === (server.level ?? null);
  return modelDone && levelDone;
}

/** The levels last picked, by model id. */
export type LevelsByModel = Readonly<Record<string, string>>;

/** The level a model is used at: the one last picked for it if it still has it, else its own. */
export function levelForModel(model: MenuModel, remembered: LevelsByModel): string | null {
  if (model.levels.length === 0) return null;
  const last = remembered[model.id];
  return last && model.levels.includes(last) ? last : model.defaultLevel;
}

export function rememberLevel(remembered: LevelsByModel, modelId: string, level: string): LevelsByModel {
  return { ...remembered, [modelId]: level };
}

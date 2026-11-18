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
  /** The agent program the model runs in ("claude" unless the model says otherwise). */
  harness: string;
}

/** The harness of a model that names none. */
export const DEFAULT_HARNESS = "claude";

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
      harness: typeof model.metadata?.harness === "string" ? model.metadata.harness : DEFAULT_HARNESS,
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

/** The menu's width when its levels need no more (and before their labels are measured). */
export const MENU_MIN_WIDTH = 272;
/** The room each side of a level's label, as a menu row's label has (its 8 px padding). */
export const LEVEL_LABEL_ROOM = 8;
/** A level cell's border, each side. */
export const LEVEL_CELL_BORDER = 1;
/** The track's padding around the cells, each side. */
export const LEVEL_TRACK_PADDING = 2;
/** The track's margin from the menu's edges, each side. */
export const LEVEL_TRACK_MARGIN = 8;
/** The gap kept between the menu and the window's edges, each side. */
const WINDOW_EDGE = 8;

/**
 * The menu's width: wide enough that every thinking cell holds the widest level label as it renders (`labelWidth`,
 * measured: it differs by language and font) with a menu row's room each side, since the cells are equal. Never
 * narrower than MENU_MIN_WIDTH and never wider than the window. `labelWidth` null: not measured yet.
 */
export function menuWidth(labelWidth: number | null, positions: number, windowWidth: number): number {
  const cell = (labelWidth ?? 0) + 2 * (LEVEL_LABEL_ROOM + LEVEL_CELL_BORDER);
  const row = positions * cell + 2 * (LEVEL_TRACK_PADDING + LEVEL_TRACK_MARGIN);
  const wanted = labelWidth === null ? MENU_MIN_WIDTH : Math.max(MENU_MIN_WIDTH, Math.ceil(row));
  return Math.min(wanted, Math.max(0, windowWidth - 2 * WINDOW_EDGE));
}

/**
 * Whether picking `toId` in a conversation on `fromId` moves it to another harness. The harnesses keep separate
 * sessions, so the conversation so far is carried over as a summary: the person is asked first.
 */
export function movesHarness(models: MenuModel[], fromId: string | null | undefined, toId: string): boolean {
  const from = menuState(models, fromId, null).model;
  const to = models.find((entry) => entry.id === toId);
  return from !== null && to !== undefined && from.harness !== to.harness;
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

/**
 * The model of the reply that is running, when it is not the conversation's (picked) model: a model picked during a
 * reply is the next reply's, so until this one ends the menu shows both (Deyao, 2026-11-16: "Fable 5.2 → Opus 5.5").
 * `runtimeModelId` is the server's word for the running reply's model. Null: nothing runs, or one model.
 */
export function runningModel(
  models: MenuModel[],
  running: boolean,
  runtimeModelId: string | null | undefined,
  shownModelId: string | null | undefined,
): MenuModel | null {
  if (!running || !runtimeModelId) return null;
  const shown = menuState(models, shownModelId, null).model;
  if (!shown || shown.id === runtimeModelId) return null;
  return models.find((entry) => entry.id === runtimeModelId) ?? null;
}

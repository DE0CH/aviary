import { useCallback, useEffect, useMemo, useState } from "react";
import { Text, useWindowDimensions, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { ChevronDown, Info } from "lucide-react-native";
import type { DraftAgentControlsProps } from "@/composer/agent-controls";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { isWeb } from "@/constants/platform";
import { useToast } from "@/contexts/toast-context";
import { useProvidersSnapshot } from "@/hooks/use-providers-snapshot";
import { useSessionStore } from "@/stores/session-store";
import type { Theme } from "@/styles/theme";
import { densityIconProps } from "@/styles/density";
import { touchSize } from "@/components/ui/control-geometry";
import { toErrorMessage } from "@/utils/error-messages";
import { useHarnessSwitchStore } from "@/yyc/harness-switch-store";
import { useModelPickAvailable } from "@/yyc/me";
import {
  levelForModel,
  menuDefaults,
  menuModels,
  menuState,
  menuWidth,
  movesHarness,
  pickSettled,
  rememberLevel,
  runningModel,
  shownChoice,
  type LevelsByModel,
  type MenuModel,
  type PendingPick,
} from "@/yyc/model-menu-state";
import { LevelLabelProbe, ThinkingLevels } from "@/yyc/model-menu-levels";

const ThemedInfo = withUnistyles(Info);
const ThemedChevron = withUnistyles(ChevronDown);
const mutedColor = (theme: { colors: { foregroundMuted: string } }) => ({ color: theme.colors.foregroundMuted });
// a size up on a phone, where the button and the (i) are a finger's 44 px
const mutedSmGlyph = densityIconProps("sm", mutedColor as (theme: Theme) => { color: string });

interface Choice {
  models: MenuModel[];
  modelId: string | null | undefined;
  level: string | null | undefined;
  selectModel: (modelId: string) => void;
  selectLevel: (level: string) => void;
  /** The running reply's model, when a model picked during the reply waits for the next one. */
  running?: MenuModel | null;
}

// The level last picked for each model (Deyao, 2026-11-13: a thinking level per model), kept on this device.
const LEVELS_KEY = "yyc.levels-by-model";
let levelsByModel: LevelsByModel | null = null;
function rememberedLevels(): LevelsByModel {
  if (levelsByModel) return levelsByModel;
  let stored: unknown = null;
  try {
    stored = JSON.parse(globalThis.localStorage?.getItem(LEVELS_KEY) ?? "null");
  } catch {
    stored = null;
  }
  const levels: Record<string, string> = {};
  if (stored && typeof stored === "object") {
    for (const [id, level] of Object.entries(stored)) if (typeof level === "string") levels[id] = level;
  }
  levelsByModel = levels;
  return levels;
}
function rememberPickedLevel(modelId: string | null | undefined, level: string): void {
  if (!modelId) return;
  levelsByModel = rememberLevel(rememberedLevels(), modelId, level);
  try {
    globalThis.localStorage?.setItem(LEVELS_KEY, JSON.stringify(levelsByModel));
  } catch {
    // no storage (private mode, native): remembered for this visit only
  }
}

/** How long a pick is shown without the server's word before the server's state shows again. */
const PICK_SHOWN_MS = 10_000;

/** A draft that has been put on the defaults once (a draft's form otherwise opens on the last choice made). */
const startedDrafts = new Set<string>();

/** A new conversation's choice: the draft's form, started on the default model at its own level. */
function useDraftChoice(agentId: string, draft: DraftAgentControlsProps): Choice {
  const models = useMemo(() => menuModels(draft.models), [draft.models]);
  const { onSelectModel, onSelectThinkingOption, selectedModel, selectedThinkingOptionId } = draft;
  useEffect(() => {
    if (models.length === 0 || startedDrafts.has(agentId)) return;
    startedDrafts.add(agentId);
    const start = menuDefaults(models);
    if (start.modelId && start.modelId !== selectedModel) onSelectModel(start.modelId);
    if (start.level && start.level !== selectedThinkingOptionId) onSelectThinkingOption(start.level);
  }, [agentId, models, onSelectModel, onSelectThinkingOption, selectedModel, selectedThinkingOptionId]);
  // each model at the level last picked for it
  const selectModel = useCallback(
    (modelId: string) => {
      onSelectModel(modelId);
      const model = models.find((entry) => entry.id === modelId);
      const level = model ? levelForModel(model, rememberedLevels()) : null;
      if (level) onSelectThinkingOption(level);
    },
    [models, onSelectModel, onSelectThinkingOption],
  );
  const selectLevel = useCallback(
    (level: string) => {
      rememberPickedLevel(menuState(models, selectedModel, null).model?.id, level);
      onSelectThinkingOption(level);
    },
    [models, onSelectThinkingOption, selectedModel],
  );
  return { models, modelId: selectedModel, level: selectedThinkingOptionId, selectModel, selectLevel };
}

/** A conversation's choice: kept with the conversation, used from its next message on. */
function useConversationChoice(serverId: string, agentId: string): Choice {
  const toast = useToast();
  const agent = useSessionStore((state) => state.sessions[serverId]?.agents?.get(agentId) ?? null);
  const client = useSessionStore((state) => state.sessions[serverId]?.client ?? null);
  const { entries } = useProvidersSnapshot(serverId, { cwd: agent?.cwd });
  const provider = agent?.provider;
  const models = useMemo(
    () => menuModels((entries ?? []).find((entry) => entry.provider === provider)?.models),
    [entries, provider],
  );
  // A pick shows the moment it is made (the server confirms a moment later; showing only its state made a click look
  // dead), and is let go when the server's state is the pick, or after a while when it never becomes it.
  const server = useMemo(
    () => ({ modelId: agent?.model, level: agent?.thinkingOptionId }),
    [agent?.model, agent?.thinkingOptionId],
  );
  const [pending, setPending] = useState<PendingPick | null>(null);
  useEffect(() => {
    if (!pending) return undefined;
    if (pickSettled(server, pending)) {
      setPending(null);
      return undefined;
    }
    const timer = setTimeout(() => setPending(null), PICK_SHOWN_MS);
    return () => clearTimeout(timer);
  }, [pending, server]);
  const refused = useCallback(
    (error: unknown) => {
      setPending(null);
      toast.error(toErrorMessage(error));
    },
    [toast],
  );
  const shown = shownChoice(server, pending);
  const hasMessages = agent?.lastUserMessageAt != null;
  const selectModel = useCallback(
    (modelId: string) => {
      if (!client) return;
      // each model at the level last picked for it
      const model = models.find((entry) => entry.id === modelId);
      const level = model ? levelForModel(model, rememberedLevels()) : null;
      const pick = () => {
        setPending({ modelId, ...(level ? { level } : {}) });
        client
          .setAgentModel(agentId, modelId)
          .then(() => (level ? client.setAgentThinkingOption(agentId, level) : undefined))
          .catch(refused);
      };
      // a model of another harness starts from a summary of the conversation: asked first, once it has messages
      if (model && hasMessages && movesHarness(models, shown.modelId, modelId)) {
        useHarnessSwitchStore.getState().ask({ agentId, modelLabel: model.label, confirm: pick });
        return;
      }
      pick();
    },
    [agentId, client, hasMessages, models, refused, shown.modelId],
  );
  const selectLevel = useCallback(
    (level: string) => {
      if (!client) return;
      rememberPickedLevel(menuState(models, shown.modelId, null).model?.id, level);
      setPending((before) => ({ ...before, level }));
      client.setAgentThinkingOption(agentId, level).catch(refused);
    },
    [agentId, client, models, refused, shown.modelId],
  );
  // a model picked during a reply is the next reply's: until this one ends, the model it runs on shows before it
  const running = runningModel(models, agent?.status === "running", agent?.runtimeInfo?.model, shown.modelId);
  return { models, modelId: shown.modelId, level: shown.level, selectModel, selectLevel, running };
}

function DraftModelMenu({ agentId, draft }: { agentId: string; draft: DraftAgentControlsProps }) {
  return <ModelMenu choice={useDraftChoice(agentId, draft)} />;
}

function ConversationModelMenu({ serverId, agentId }: { serverId: string; agentId: string }) {
  return <ModelMenu choice={useConversationChoice(serverId, agentId)} />;
}

/**
 * Where Paseo's composer shows its model picker: one button naming the model and the thinking level, opening a menu of
 * the models and, under them, the thinking levels (docs/paseo-ui.md). The thinking row has the same positions for every
 * model; a level the chosen model lacks is disabled where it stands. Where the deployment does not let people pick
 * (yyc/me.ts `modelPick`), it is the model's name alone.
 */
export function YycModelMenu(props: { serverId: string; agentId: string; draft: DraftAgentControlsProps | undefined }) {
  if (props.draft) return <DraftModelMenu agentId={props.agentId} draft={props.draft} />;
  return <ConversationModelMenu serverId={props.serverId} agentId={props.agentId} />;
}

function ModelMenu({ choice }: { choice: Choice }) {
  const { t } = useTranslation();
  const canPick = useModelPickAvailable();
  const { width: windowWidth } = useWindowDimensions();
  const state = menuState(choice.models, choice.modelId, choice.level);
  const levelLabel = useCallback((level: string) => t(`conversation.model.level.${level}`, { defaultValue: level }), [t]);
  // the widest level label as it renders (LevelLabelProbe), which the menu's width follows
  const [labelWidth, setLabelWidth] = useState<number | null>(null);
  const rowKey = state.row.join(" ");
  const rowLabels = useMemo(() => (rowKey ? rowKey.split(" ").map(levelLabel) : []), [rowKey, levelLabel]);
  // A question about the pick (another harness) closes the menu: left open, its backdrop lay over the question and the
  // first tap on 繼續 or 取消 only closed the menu (seen on a phone, 2026-11-18).
  const [open, setOpen] = useState(false);
  const asking = useHarnessSwitchStore((store) => store.question !== null);
  useEffect(() => {
    if (asking) setOpen(false);
  }, [asking]);
  if (!state.model) return null;
  const model = state.model;
  if (!canPick) {
    // where people do not pick models, the model's name alone, as before
    return (
      <View style={styles.trigger} testID="yyc-model-label">
        <Text style={styles.label} numberOfLines={1}>
          {model.label}
        </Text>
      </View>
    );
  }
  const running = choice.running ?? null;
  const picked = state.level ? `${model.label} · ${levelLabel(state.level)}` : model.label;
  const summary = running ? t("conversation.model.runningThenPicked", { running: running.label, picked }) : picked;
  return (
    <DropdownMenu open={open} onOpenChange={setOpen}>
      <DropdownMenuTrigger
        testID="yyc-model-menu"
        style={styles.trigger}
        accessibilityRole="button"
        accessibilityLabel={t("conversation.model.menuLabel", { model: summary })}
      >
        {running ? (
          <Text style={styles.running} numberOfLines={1} testID="yyc-model-running">
            {`${running.label} →`}
          </Text>
        ) : null}
        <Text style={styles.label} numberOfLines={1}>
          {model.label}
        </Text>
        {state.level ? (
          <Text style={styles.level} numberOfLines={1}>
            {`· ${levelLabel(state.level)}`}
          </Text>
        ) : null}
        <ThemedChevron uniProps={mutedSmGlyph} />
      </DropdownMenuTrigger>
      {rowLabels.length > 0 ? <LevelLabelProbe labels={rowLabels} onWidest={setLabelWidth} /> : null}
      <DropdownMenuContent
        side="top"
        align="start"
        width={menuWidth(labelWidth, state.row.length, windowWidth)}
        sheetTitle={t("conversation.model.title")}
      >
        <View style={styles.heading}>
          <Text style={styles.headingText}>{t("conversation.model.title")}</Text>
          <Tooltip delayDuration={150} enabledOnDesktop enabledOnMobile>
            <TooltipTrigger
              accessibilityRole="button"
              accessibilityLabel={t("conversation.model.infoLabel")}
              hitSlop={isWeb ? undefined : 8}
              style={styles.info}
            >
              <ThemedInfo uniProps={mutedSmGlyph} />
            </TooltipTrigger>
            <TooltipContent side="top">
              <Text style={styles.tooltipText}>{t("conversation.model.info")}</Text>
            </TooltipContent>
          </Tooltip>
        </View>
        {state.models.map((entry) => (
          <DropdownMenuItem
            key={entry.id}
            testID={`yyc-model-${entry.id}`}
            selected={entry.id === model.id}
            active={entry.id === model.id}
            closeOnSelect={false}
            onSelect={() => choice.selectModel(entry.id)}
          >
            {entry.label}
          </DropdownMenuItem>
        ))}
        {state.row.length > 0 ? (
          <>
            <DropdownMenuSeparator />
            <View style={styles.heading}>
              <Text style={styles.headingText}>{t("conversation.model.thinking")}</Text>
              {model.levels.length === 0 ? (
                <Text style={styles.headingNote}>{t("conversation.model.thinkingUnavailable")}</Text>
              ) : null}
            </View>
            <ThinkingLevels
              row={state.row}
              offered={model.levels}
              picked={state.level}
              label={levelLabel}
              onPick={choice.selectLevel}
            />
          </>
        ) : null}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

const styles = StyleSheet.create((theme) => ({
  trigger: {
    // the composer's left group aligns its children to the bottom, which put this one line of text below the centre of
    // the attach button (+) beside it (Deyao, 2026-10-27): centred on the button instead
    alignSelf: "center",
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
    paddingHorizontal: theme.spacing[2],
    borderRadius: theme.borderRadius.md,
    // The composer's control size (composer/input/presentation.tsx CONTROL_SIZE): a finger's 44 px on a phone.
    minHeight: touchSize(28),
    minWidth: 0,
    flexShrink: 1,
  },
  // The model chip it replaces (composer/agent-controls/control.tsx toolbarValue).
  label: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.normal,
    flexShrink: 1,
    minWidth: 0,
  },
  // the running reply's model, before the picked one: quieter than it, and the first to give way in a narrow composer
  running: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.normal,
    opacity: 0.6,
    flexShrink: 2,
    minWidth: 0,
  },
  level: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.normal,
    flexShrink: 0,
  },
  heading: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingHorizontal: theme.spacing[3],
    // on a phone the (i)'s 44 px box sets the heading's height
    paddingTop: { xs: 0, md: theme.spacing[2] },
    paddingBottom: { xs: 0, md: theme.spacing[1] },
  },
  // the (i): a finger's 44 px box on a phone, its glyph kept beside the heading's word
  info: {
    width: touchSize(18),
    height: touchSize(18),
    marginLeft: { xs: -13, md: 0 },
    alignItems: "center",
    justifyContent: "center",
  },
  headingText: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  headingNote: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    marginLeft: "auto",
  },
  tooltipText: {
    color: theme.colors.popoverForeground,
    fontSize: theme.fontSize.sm,
    maxWidth: 260,
  },
}));

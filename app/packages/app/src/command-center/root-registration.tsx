import { useMemo } from "react";
import { router, type Href } from "expo-router";
import { useTranslation } from "react-i18next";
import { Plus, Settings, Trash2 } from "lucide-react-native";
import { withUnistyles } from "react-native-unistyles";
import { useKeyboardActionDispatcher } from "@/keyboard/keyboard-action-dispatcher-context";
import { clearCommandCenterFocusRestoreElement } from "@/utils/command-center-focus-restore";
import { buildSessionsRoute, buildSettingsRoute } from "@/utils/host-routes";
import type { CommandCenterContribution, CommandCenterIconProps } from "./contributions";
import { useCommandCenterActions } from "./provider";

const ThemedPlus = withUnistyles(Plus, (theme) => ({ color: theme.colors.foregroundMuted }));
const ThemedTrash = withUnistyles(Trash2, (theme) => ({ color: theme.colors.foregroundMuted }));
const ThemedSettings = withUnistyles(Settings, (theme) => ({
  color: theme.colors.foregroundMuted,
}));

// The app's default stroke (LucideProvider), like the sidebar's icons.
function PlusIcon({ size }: CommandCenterIconProps) {
  return <ThemedPlus size={size} />;
}

function TrashIcon({ size }: CommandCenterIconProps) {
  return <ThemedTrash size={size} />;
}

function SettingsIcon({ size }: CommandCenterIconProps) {
  return <ThemedSettings size={size} />;
}

/**
 * The search palette's actions: the sidebar's own items (新對話, 最近刪除) and 設定. Paseo's developer actions (add
 * project, import a CLI session, keyboard shortcuts, home, grouping, sidebar toggles) are not offered
 * (docs/paseo-ui.md).
 */
export function CommandCenterRootActions() {
  const keyboardActionDispatcher = useKeyboardActionDispatcher();
  const { t } = useTranslation();
  const settingsRoute = useMemo<Href>(() => buildSettingsRoute(), []);
  const sessionsRoute = useMemo<Href>(() => buildSessionsRoute(), []);
  const actions = useMemo<CommandCenterContribution[]>(
    () => [
      {
        id: "new-workspace",
        group: "actions",
        groupRank: 0,
        rank: 1,
        keywords: ["new", "chat", "conversation", "新對話"],
        visibility: "always",
        run: () => {
          keyboardActionDispatcher.dispatch({ id: "workspace.new", scope: "sidebar" });
        },
        presentation: {
          kind: "action",
          title: t("sidebar.actions.newWorkspace"),
          sectionTitle: t("shell.commandCenter.actions"),
          icon: PlusIcon,
        },
      },
      {
        id: "history",
        group: "actions",
        groupRank: 0,
        rank: 4,
        keywords: ["history", "deleted", "recent", "最近刪除"],
        visibility: "always",
        run: () => {
          clearCommandCenterFocusRestoreElement();
          router.push(sessionsRoute);
        },
        presentation: {
          kind: "action",
          title: t("sidebar.sections.sessions"),
          sectionTitle: t("shell.commandCenter.actions"),
          icon: TrashIcon,
        },
      },
      {
        id: "settings",
        group: "actions",
        groupRank: 0,
        rank: 6,
        keywords: ["settings", "preferences", "設定"],
        visibility: "always",
        run: () => {
          clearCommandCenterFocusRestoreElement();
          router.push(settingsRoute);
        },
        presentation: {
          kind: "action",
          title: t("sidebar.actions.settings"),
          sectionTitle: t("shell.commandCenter.actions"),
          icon: SettingsIcon,
        },
      },
    ],
    [keyboardActionDispatcher, sessionsRoute, settingsRoute, t],
  );

  useCommandCenterActions({ sourceId: "root", enabled: true, actions });
  return null;
}

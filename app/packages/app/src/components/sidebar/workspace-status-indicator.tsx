// The status spot at the start of a conversation's row in the sidebar: the ring while it works, the alert while it
// waits for an answer, and (Deyao, 2026-11-17, the chat's design A) the blue dot while background tasks run after the
// reply, the clock while a schedule is armed, the grey dot otherwise. Its own module so a test can draw it without the
// row's hooks (lessons/116: a browser test that reaches the host runtime breaks Vite's dependency scan).
import { View, type ViewStyle } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { CircleAlert, Clock, Folder, FolderGit2, Monitor } from "lucide-react-native";
import { useTranslation } from "react-i18next";
import type { SidebarWorkspaceEntry } from "@/hooks/sidebar-workspaces-view-model";
import { densityIconBox, SIDEBAR_ROW_LINE_HEIGHT } from "@/styles/density";
import type { Theme } from "@/styles/theme";
import type { SidebarStateBucket } from "@/utils/sidebar-agent-state";
import { getStatusDotColor } from "@/utils/status-dot-color";
import {
  STATUS_INDICATOR_ALERT_SIZE,
  STATUS_INDICATOR_BACKGROUND_DOT_SIZE,
  STATUS_INDICATOR_DOT_SIZE,
  STATUS_INDICATOR_FILLED_DOT_SIZE,
} from "@/utils/status-indicator-geometry";
import { shouldRenderSyncedStatusLoader } from "@/utils/status-loader";
import { StatusRing } from "@/components/status-ring";
import { DensityIcon } from "@/components/ui/density-icon";

const TITLE_LINE_HEIGHT = SIDEBAR_ROW_LINE_HEIGHT;

const needsInputColorMapping = (theme: Theme) => ({
  color: theme.colors.surface0,
  fill: getStatusDotColor({ theme, bucket: "needs_input" }) ?? undefined,
});

const ThemedCircleAlert = withUnistyles(CircleAlert);
const ThemedClock = withUnistyles(Clock);
const scheduledColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

/** The next wakeup's time as the list's label says it: the time today, the weekday and time on another day. */
export function formatNextWakeup(at: Date, language: string, now: Date = new Date()): string {
  const sameDay = at.toDateString() === now.toDateString();
  return new Intl.DateTimeFormat(language, {
    ...(sameDay ? {} : { weekday: "short" as const }),
    hour: "2-digit",
    minute: "2-digit",
  }).format(at);
}

export function WorkspaceStatusIndicator({
  bucket,
  backgroundTasks = 0,
  nextScheduleAt = null,
  workspaceKind,
  loading = false,
  reserveIdleSpace = true,
}: {
  bucket: SidebarWorkspaceEntry["statusBucket"];
  /** Background tasks still running after the reply ended. */
  backgroundTasks?: number;
  /** When the soonest armed schedule fires next. */
  nextScheduleAt?: Date | null;
  workspaceKind: SidebarWorkspaceEntry["workspaceKind"];
  loading?: boolean;
  reserveIdleSpace?: boolean;
}) {
  const { t, i18n } = useTranslation();
  // Busy is the only status that moves, and it is the ring rather than a dot for the same
  // reason it is a dot elsewhere: every status in the sidebar sits in this one slot, so busy
  // has to fill it without displacing anything. A row starting up and a row working are both
  // busy, so they share the ring and differ only in testID.
  if (loading) {
    return (
      <View style={styles.workspaceStatusDot} testID="workspace-status-indicator-loading">
        <StatusRing />
      </View>
    );
  }

  if (shouldRenderSyncedStatusLoader({ bucket })) {
    return (
      <View style={styles.workspaceStatusDot} testID="workspace-status-indicator-running">
        <StatusRing />
      </View>
    );
  }

  if (bucket === "needs_input") {
    return (
      <View style={styles.workspaceStatusDot} testID="workspace-status-indicator-needs_input">
        <ThemedCircleAlert size={STATUS_INDICATOR_ALERT_SIZE} uniProps={needsInputColorMapping} />
      </View>
    );
  }

  if (bucket === "attention") {
    return (
      <View style={styles.workspaceStatusDot} testID="workspace-status-indicator-attention">
        <View style={styles.standaloneStatusDot} />
      </View>
    );
  }

  // Idle, but not finished with (Deyao, 2026-11-17, the chat's design A): one mark in the same slot. Background
  // tasks still running are a blue dot, the busy colour without the ring's motion (work goes on, nothing is being
  // written); an armed schedule is a clock. Only one fits, so background tasks win.
  if (bucket === "done" && backgroundTasks > 0) {
    return (
      <View
        style={styles.workspaceStatusDot}
        testID="workspace-status-indicator-background"
        accessibilityLabel={t("conversation.listMarks.background", { count: backgroundTasks })}
      >
        <View style={styles.backgroundStatusDot} />
      </View>
    );
  }

  if (bucket === "done" && nextScheduleAt) {
    return (
      <View
        style={styles.workspaceStatusDot}
        testID="workspace-status-indicator-scheduled"
        accessibilityLabel={t("conversation.listMarks.scheduled", {
          time: formatNextWakeup(nextScheduleAt, i18n.language),
        })}
      >
        <ThemedClock size={STATUS_INDICATOR_ALERT_SIZE} uniProps={scheduledColorMapping} />
      </View>
    );
  }

  if (bucket === "done") {
    // An idle row still gets a dot rather than an empty slot. Nested rows are marked as
    // workspaces by indentation alone, and with nothing in the leading slot the rail has no
    // edge to read against — a workspace carrying its own glyph starts looking like a project
    // header. The dot is muted to half opacity so it holds the rail without reporting status.
    return reserveIdleSpace ? (
      <View style={styles.workspaceStatusDot} testID="workspace-status-indicator-done">
        <View style={styles.idleStatusDot} />
      </View>
    ) : null;
  }

  let KindIcon: typeof Monitor;
  if (workspaceKind === "local_checkout") KindIcon = Monitor;
  else if (workspaceKind === "worktree") KindIcon = FolderGit2;
  else KindIcon = Folder;

  const dotColorStyle = getStatusDotColorStyle(bucket);
  return (
    <View style={styles.workspaceStatusDot} testID={`workspace-status-indicator-${bucket}`}>
      <DensityIcon icon={KindIcon} size="sm" color="foregroundMuted" />
      {dotColorStyle ? <StatusDotOverlay dotColorStyle={dotColorStyle} /> : null}
    </View>
  );
}

function StatusDotOverlay({ dotColorStyle }: { dotColorStyle: ViewStyle }) {
  return <View style={[styles.statusDotOverlay, dotColorStyle]} />;
}

function getStatusDotColorStyle(bucket: SidebarStateBucket) {
  switch (bucket) {
    case "needs_input":
      return styles.statusDotNeedsInput;
    case "failed":
      return styles.statusDotFailed;
    case "running":
      return styles.statusDotRunning;
    case "attention":
      return styles.statusDotAttention;
    case "done":
      return null;
  }
}

const styles = StyleSheet.create((theme) => ({
  workspaceStatusDot: {
    position: "relative",
    width: densityIconBox("md"),
    height: TITLE_LINE_HEIGHT,
    borderRadius: theme.borderRadius.full,
    flexShrink: 0,
    alignItems: "center",
    justifyContent: "center",
  },
  statusDotOverlay: {
    position: "absolute",
    right: 0,
    bottom: 0,
    width: STATUS_INDICATOR_DOT_SIZE,
    height: STATUS_INDICATOR_DOT_SIZE,
    borderRadius: theme.borderRadius.full,
    borderWidth: 1,
  },
  standaloneStatusDot: {
    width: STATUS_INDICATOR_FILLED_DOT_SIZE,
    height: STATUS_INDICATOR_FILLED_DOT_SIZE,
    borderRadius: theme.borderRadius.full,
    backgroundColor: getStatusDotColor({ theme, bucket: "attention" }) ?? undefined,
  },
  backgroundStatusDot: {
    width: STATUS_INDICATOR_BACKGROUND_DOT_SIZE,
    height: STATUS_INDICATOR_BACKGROUND_DOT_SIZE,
    borderRadius: theme.borderRadius.full,
    backgroundColor: getStatusDotColor({ theme, bucket: "running" }) ?? undefined,
  },
  idleStatusDot: {
    width: STATUS_INDICATOR_FILLED_DOT_SIZE,
    height: STATUS_INDICATOR_FILLED_DOT_SIZE,
    borderRadius: theme.borderRadius.full,
    backgroundColor: theme.colors.foregroundExtraMuted,
    opacity: 0.3,
  },
  statusDotNeedsInput: {
    backgroundColor: getStatusDotColor({ theme, bucket: "needs_input" }) ?? undefined,
    borderColor: theme.colors.surface0,
  },
  statusDotFailed: {
    backgroundColor: getStatusDotColor({ theme, bucket: "failed" }) ?? undefined,
    borderColor: theme.colors.surface0,
  },
  statusDotRunning: {
    backgroundColor: getStatusDotColor({ theme, bucket: "running" }) ?? undefined,
    borderColor: theme.colors.surface0,
  },
  statusDotAttention: {
    backgroundColor: getStatusDotColor({ theme, bucket: "attention" }) ?? undefined,
    borderColor: theme.colors.surface0,
  },
}));

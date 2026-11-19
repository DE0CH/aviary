import { Suspense, useCallback } from "react";
import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { StyleSheet } from "react-native-unistyles";
import { buildWorkspaceAttachmentScopeKey } from "@/attachments/workspace-attachments-store";
import { useToast } from "@/contexts/toast-context";
import { useCheckoutGitActionsStore } from "@/git/actions-store";
import { ChunkErrorBoundary } from "@/components/loading-frame";
import { PullRequestPaneError } from "@/git/pull-request-panel/pane-error";
import { PullRequestPaneSkeleton } from "@/git/pull-request-panel/pane-skeleton";
import { lazyWithRetry } from "@/utils/lazy-with-retry";
import type { UsePrPaneDataResult } from "@/git/pull-request-panel/use-data";

import { resolvePullRequestContentState } from "./state";
export {
  useWorkspacePullRequestDiscovery,
  useHasPullRequest,
  usePullRequestAutoAdd,
} from "./discovery";
export { usePullRequestData } from "./data";

// The pull request view (its timeline, markdown, rich copy) is its own chunk, fetched when one opens
// (docs/paseo-ui.md "Loading").
const PullRequestPane = lazyWithRetry(
  () => import("@/git/pull-request-panel/pane"),
  (m) => m.PullRequestPane,
);

export function PullRequestContent(input: {
  serverId: string;
  workspaceId?: string | null;
  cwd: string;
  prPane: UsePrPaneDataResult;
}) {
  const { t } = useTranslation();
  const toast = useToast();
  const refresh = useCheckoutGitActionsStore((state) => state.refresh);
  const onRetry = useCallback(() => {
    void refresh({ serverId: input.serverId, cwd: input.cwd }).catch((error) => {
      toast.error(error instanceof Error ? error.message : t("workspace.git.diff.failedRefresh"));
    });
  }, [input.cwd, input.serverId, refresh, t, toast]);

  if (input.prPane.data) {
    return (
      <ChunkErrorBoundary name="pull-request" fallback={<PullRequestPaneError onRetry={onRetry} />}>
        <Suspense fallback={<PullRequestPaneSkeleton />}>
          <PullRequestPane
            serverId={input.serverId}
            cwd={input.cwd}
            data={input.prPane.data}
            activityLoading={input.prPane.activityLoading}
            workspaceAttachmentScopeKey={buildWorkspaceAttachmentScopeKey({
              serverId: input.serverId,
              workspaceId: input.workspaceId,
              cwd: input.cwd,
            })}
          />
        </Suspense>
      </ChunkErrorBoundary>
    );
  }
  const contentState = resolvePullRequestContentState(input.prPane);
  if (contentState === "error") {
    return <PullRequestPaneError onRetry={onRetry} />;
  }
  if (contentState === "loading") {
    return <PullRequestPaneSkeleton />;
  }
  return (
    <View style={styles.empty} testID="pull-request-empty-state">
      <Text style={styles.emptyTitle}>{t("panels.pullRequest.emptyTitle")}</Text>
      <Text style={styles.emptyDescription}>{t("panels.pullRequest.emptyDescription")}</Text>
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  empty: {
    flex: 1,
    alignItems: "center",
    justifyContent: "center",
    gap: 8,
    padding: 24,
  },
  emptyTitle: {
    color: theme.colors.foreground,
  },
  emptyDescription: {
    color: theme.colors.foregroundMuted,
  },
}));

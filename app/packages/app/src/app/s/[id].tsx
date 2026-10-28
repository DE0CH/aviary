import React, { useCallback, useEffect, useMemo, useState } from "react";
import { Text, View } from "react-native";
import { router, useLocalSearchParams } from "expo-router";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import type { AgentTimelineItem } from "@getpaseo/protocol/agent-types";
import type { AgentStreamEventPayload } from "@getpaseo/protocol/messages";
import { AgentStreamView } from "@/agent-stream/view";
import { BackHeader } from "@/components/headers/back-header";
import { Button } from "@/components/ui/button";
import { LoadingSpinner } from "@/components/ui/loading-spinner";
import type { AgentScreenAgent } from "@/hooks/use-agent-screen-state-machine";
import { useHosts } from "@/runtime/host-runtime";
import type { TurnPresentation } from "@/timeline/turn-liveness";
import type { PendingPermission } from "@/types/shared";
import { hydrateStreamState, type StreamItem } from "@/types/stream";
import { buildHostWorkspaceRoute } from "@/utils/host-routes";
import { api, ApiError, apiUrl } from "@/yyc/api";
import { ConversationFilesContext, fileQuery } from "@/yyc/sent-files";
import type { Theme } from "@/styles/theme";

const ThemedLoadingSpinner = withUnistyles(LoadingSpinner);
const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

/** One row of a conversation's timeline, as the bridge builds it (server/modules/paseo/paseo-timeline.ts). */
interface ShareTimelineRow {
  provider: string;
  item: AgentTimelineItem;
  turnId?: string;
  timestamp: string;
  seqStart: number;
  seqEnd: number;
}

/** GET /api/jarvis/shares/:id (DE0CH/claudecodeui server/modules/jarvis/jarvis-shares.routes.ts). */
interface ShareView {
  id: string;
  title: string;
  ownerName: string;
  isOwner: boolean;
  createdAt: string;
  /** The conversation as the owner saw it then, as the chat view's timeline rows. */
  timeline: ShareTimelineRow[];
}

const EMPTY_ITEMS: StreamItem[] = [];
const NO_PERMISSIONS = new Map<string, PendingPermission>();
const FINISHED: TurnPresentation = { isActive: false, isCancelling: false, startedAt: null, turnId: null };

/** The rows as the chat's stream items, through the same reducer a conversation's history goes through
 * (timeline/session-stream-reducers.ts processTimelineResponse). */
function shareStreamItems(view: ShareView): StreamItem[] {
  const epoch = `share-${view.id}`;
  return hydrateStreamState(
    view.timeline.map((row) => ({
      event: {
        type: "timeline",
        provider: row.provider,
        item: row.item,
        ...(row.turnId ? { turnId: row.turnId } : {}),
      } as AgentStreamEventPayload,
      timestamp: new Date(row.timestamp),
      timelineCursor: { epoch, seq: row.seqEnd },
    })),
    { source: "canonical" },
  );
}

/**
 * A shared conversation, read only (「<name> 分享的對話（只讀）」), for staff signed in to the instance. It renders with
 * the chat's own view (AgentStreamView, read only), its files read through the share; its one action, 「繼續對話」,
 * makes the viewer a conversation of their own from it (docs/paseo-ui.md).
 */
export default function SharedConversationRoute() {
  const { t } = useTranslation();
  const params = useLocalSearchParams<{ id?: string }>();
  const id = typeof params.id === "string" ? params.id : "";
  const hosts = useHosts();
  const [view, setView] = useState<ShareView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [continuing, setContinuing] = useState(false);
  const [continueError, setContinueError] = useState<string | null>(null);

  useEffect(() => {
    if (!id) return;
    api<ShareView>(`/api/jarvis/shares/${encodeURIComponent(id)}`)
      .then(setView)
      .catch((e) =>
        setError(
          e instanceof ApiError && e.status === 404
            ? t("conversation.shared.gone")
            : t("conversation.shared.loadFailed"),
        ),
      );
  }, [id, t]);

  const streamItems = useMemo(() => (view ? shareStreamItems(view) : EMPTY_ITEMS), [view]);
  const serverId = hosts[0]?.serverId ?? "";
  const context = useMemo<AgentScreenAgent>(
    () => ({ serverId, id: view ? `share-${view.id}` : "share", provider: "claude", status: "idle", cwd: "/workspace" }),
    [serverId, view],
  );
  const fileUrl = useCallback(
    (relativePath: string, inline: boolean, sent?: string) =>
      apiUrl(`/api/jarvis/shares/${encodeURIComponent(id)}/files/raw?${fileQuery(relativePath, inline, sent)}`),
    [id],
  );

  const handleContinue = useCallback(async () => {
    setContinuing(true);
    try {
      const body = await api<{ sessionId: string }>(`/api/jarvis/shares/${encodeURIComponent(id)}/continue`, {
        method: "POST",
      });
      const serverId = hosts[0]?.serverId;
      router.replace(serverId ? buildHostWorkspaceRoute(serverId, `wks_${body.sessionId}`) : "/");
    } catch {
      setContinueError(t("conversation.shared.continueFailed"));
      setContinuing(false);
    }
  }, [hosts, id, t]);
  const goToChats = useCallback(() => router.replace("/new"), []);

  if (error) {
    return (
      <View style={styles.center}>
        <Text style={styles.errorText}>{error}</Text>
        <Button variant="secondary" size="sm" onPress={goToChats}>
          {t("conversation.shared.back")}
        </Button>
      </View>
    );
  }
  if (!view) {
    return (
      <View style={styles.center}>
        <ThemedLoadingSpinner size="small" uniProps={mutedColorMapping} />
      </View>
    );
  }

  return (
    <View style={styles.screen}>
      {/* The app's own header (back to the conversations), as on 最近刪除 and 設定. */}
      <BackHeader
        title={view.title}
        onBack={goToChats}
        rightContent={
          <Button variant="default" size="sm" onPress={() => void handleContinue()} disabled={continuing}>
            {t("conversation.shared.continue")}
          </Button>
        }
      />
      <View style={styles.byline}>
        <Text style={styles.muted}>{t("conversation.shared.byline", { name: view.ownerName })}</Text>
        {continueError ? <Text style={styles.errorText}>{continueError}</Text> : null}
      </View>
      <View style={styles.stream}>
        <ConversationFilesContext.Provider value={fileUrl}>
          <AgentStreamView
            agentId={context.id}
            serverId={serverId}
            context={context}
            streamItems={streamItems}
            turnPresentation={FINISHED}
            pendingPermissions={NO_PERMISSIONS}
            isAuthoritativeHistoryReady
            readOnly
          />
        </ConversationFilesContext.Provider>
      </View>
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  screen: {
    flex: 1,
    backgroundColor: theme.colors.surface0,
  },
  center: {
    flex: 1,
    alignItems: "center",
    justifyContent: "center",
    backgroundColor: theme.colors.surface0,
    padding: theme.spacing[6],
    gap: theme.spacing[4],
  },
  // The chat column's width and gutters (agent-stream view.tsx), above the stream.
  byline: {
    width: "100%",
    maxWidth: theme.contentMaxWidth,
    alignSelf: "center",
    paddingHorizontal: { xs: theme.spacing[4], md: theme.spacing[6] },
    paddingTop: theme.spacing[3],
    gap: theme.spacing[2],
  },
  muted: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  errorText: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    lineHeight: 20,
    textAlign: "center",
  },
  stream: {
    flex: 1,
    minHeight: 0,
  },
}));

import React, { useCallback, useEffect, useMemo, useState } from "react";
import { ScrollView, Text, View } from "react-native";
import { router, useLocalSearchParams } from "expo-router";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { MarkdownRenderer } from "@/components/markdown/renderer";
import { Button } from "@/components/ui/button";
import { LoadingSpinner } from "@/components/ui/loading-spinner";
import { useHosts } from "@/runtime/host-runtime";
import { buildHostWorkspaceRoute } from "@/utils/host-routes";
import { api, ApiError, apiUrl } from "@/yyc/api";
import { SentFilesCards, type SentFile } from "@/yyc/sent-files";
import type { Theme } from "@/styles/theme";
import { BackHeader } from "@/components/headers/back-header";

const ThemedLoadingSpinner = withUnistyles(LoadingSpinner);
const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

/** GET /api/jarvis/shares/:id (DE0CH/claudecodeui server/modules/jarvis/jarvis-shares.routes.ts). */
interface ShareView {
  id: string;
  title: string;
  ownerName: string;
  isOwner: boolean;
  createdAt: string;
  /** The conversation as the owner saw it then: CloudCLI's normalized messages. */
  messages: Array<Record<string, unknown>>;
  files: string[];
}

type ShareRow =
  | { kind: "user"; key: string; text: string }
  | { kind: "assistant"; key: string; text: string }
  | { kind: "files"; key: string; files: SentFile[]; caption: string | null };

/** The messages a viewer sees: what was said, and the files Claude sent (tool steps are left out, as before). */
function shareRows(view: ShareView): ShareRow[] {
  const rows: ShareRow[] = [];
  const sendCalls = new Map<string, { caption: string | null }>();
  view.messages.forEach((message, index) => {
    const key = String(message.id ?? index);
    const text = typeof message.content === "string" ? message.content : "";
    if (message.kind === "text" && text.trim()) {
      rows.push({ kind: message.role === "user" ? "user" : "assistant", key, text });
      return;
    }
    if (message.kind === "tool_use" && message.toolName === "SendUserFile" && typeof message.toolId === "string") {
      const input = message.toolInput as { caption?: unknown } | undefined;
      sendCalls.set(message.toolId, { caption: typeof input?.caption === "string" ? input.caption : null });
      return;
    }
    if (message.kind === "tool_result" && typeof message.toolId === "string" && sendCalls.has(message.toolId)) {
      const result = (message.toolResult as { toolUseResult?: unknown } | undefined)?.toolUseResult;
      const attachments = (result as { attachments?: unknown } | undefined)?.attachments;
      const files: SentFile[] = [];
      for (const entry of Array.isArray(attachments) ? attachments : []) {
        const file = entry as { path?: unknown; size?: unknown; media_type?: unknown; isImage?: unknown };
        if (typeof file.path !== "string") continue;
        const relative = file.path.replace(/^\/workspace\//, "");
        if (!view.files.includes(relative)) continue;
        files.push({
          path: relative,
          name: relative.split("/").pop() ?? relative,
          size: typeof file.size === "number" ? file.size : null,
          mediaType: typeof file.media_type === "string" ? file.media_type : null,
          isImage: file.isImage === true,
        });
      }
      if (files.length > 0) {
        rows.push({ kind: "files", key, files, caption: sendCalls.get(message.toolId)?.caption ?? null });
      }
    }
  });
  return rows;
}

/**
 * A shared conversation, read only (「<name> 分享的對話（只讀）」), for staff signed in to the instance; its one action,
 * 「繼續對話」, makes the viewer a conversation of their own from it (docs/paseo-ui.md).
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

  const rows = useMemo(() => (view ? shareRows(view) : []), [view]);

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
      <ScrollView contentContainerStyle={styles.column}>
        <Text style={styles.muted}>{t("conversation.shared.byline", { name: view.ownerName })}</Text>
        {continueError ? <Text style={styles.errorText}>{continueError}</Text> : null}
        {rows.map((row) => {
          if (row.kind === "user") {
            return (
              <View key={row.key} style={styles.userRow}>
                <View style={styles.userBubble}>
                  <Text style={styles.userText} selectable>
                    {row.text}
                  </Text>
                </View>
              </View>
            );
          }
          if (row.kind === "assistant") {
            return (
              <View key={row.key} style={styles.assistant}>
                <MarkdownRenderer text={row.text} />
              </View>
            );
          }
          return (
            <SharedFiles key={row.key} shareId={view.id} files={row.files} caption={row.caption} />
          );
        })}
      </ScrollView>
    </View>
  );
}

/** Sent-file cards whose downloads go through the share (only its own files can be read). */
function SharedFiles({ shareId, files, caption }: { shareId: string; files: SentFile[]; caption: string | null }) {
  const urlFor = useCallback(
    (file: SentFile, inline: boolean) =>
      apiUrl(
        `/api/jarvis/shares/${encodeURIComponent(shareId)}/files/raw?path=${encodeURIComponent(file.path)}${inline ? "&inline=1" : ""}`,
      ),
    [shareId],
  );
  return <SentFilesCards sessionId="" files={files} caption={caption} urlFor={urlFor} />;
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
  // The chat column (agent-stream view.tsx, strategy-web.tsx) and its messages (components/message.tsx).
  column: {
    width: "100%",
    maxWidth: theme.contentMaxWidth,
    alignSelf: "center",
    paddingHorizontal: { xs: theme.spacing[4], md: theme.spacing[6] },
    paddingVertical: theme.spacing[4],
  },
  userRow: {
    alignItems: "flex-end",
    marginTop: theme.spacing[4],
    marginBottom: theme.spacing[4],
  },
  userBubble: {
    flexShrink: 1,
    backgroundColor: theme.colors.surface3,
    borderRadius: theme.borderRadius["2xl"],
    borderTopRightRadius: theme.borderRadius.sm,
    padding: theme.spacing[4],
  },
  userText: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.content,
    lineHeight: Math.round(theme.fontSize.content * 1.4),
  },
  assistant: {
    width: "100%",
    paddingVertical: theme.spacing[3],
  },
}));

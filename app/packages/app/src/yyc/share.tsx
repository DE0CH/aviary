import React, { useCallback, useMemo, useState } from "react";
import { Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { AdaptiveModalSheet, type SheetHeader } from "@/components/adaptive-modal-sheet";
import { Button } from "@/components/ui/button";
import { useToast } from "@/contexts/toast-api-context";
import { copyText } from "@/plugins/react-native/clipboard";
import { absoluteUrl, api, ApiError, sessionIdOfWorkspace } from "@/yyc/api";

interface ShareLink {
  id: string;
  url: string;
  createdAt: string;
}

/**
 * 「分享」 (DE0CH/claudecodeui docs/paseo-ui.md, jarvis-shares.routes.ts): every copy freezes the conversation into a
 * new staff-only read-only link; the sheet lists the live links, each copyable and revocable.
 */
export function useShareConversation(input: { serverId: string | null; workspaceId: string | null }) {
  const sessionId = sessionIdOfWorkspace(input.workspaceId);
  const [visible, setVisible] = useState(false);
  const open = useCallback(() => setVisible(true), []);
  const close = useCallback(() => setVisible(false), []);
  const sheet = sessionId ? <ShareSheet visible={visible} sessionId={sessionId} onClose={close} /> : null;
  return { open, sheet };
}

function ShareSheet({ visible, sessionId, onClose }: { visible: boolean; sessionId: string; onClose: () => void }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [links, setLinks] = useState<ShareLink[] | null>(null);
  const [isCreating, setIsCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const canNativeShare = typeof navigator !== "undefined" && typeof navigator.share === "function";

  const load = useCallback(async () => {
    try {
      const body = await api<{ shares: ShareLink[] }>(`/api/jarvis/sessions/${sessionId}/shares`);
      setLinks(body.shares);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [sessionId]);

  React.useEffect(() => {
    if (!visible) return;
    setError(null);
    void load();
  }, [visible, load]);

  const createLink = useCallback(async (): Promise<string | null> => {
    setIsCreating(true);
    setError(null);
    try {
      const link = await api<ShareLink>(`/api/jarvis/sessions/${sessionId}/shares`, { method: "POST" });
      setLinks((current) => [link, ...(current ?? [])]);
      return absoluteUrl(link.url);
    } catch (e) {
      setError(
        e instanceof ApiError && e.code === "NOTHING_TO_SHARE"
          ? t("conversation.share.nothingYet")
          : e instanceof Error
            ? e.message
            : String(e),
      );
      return null;
    } finally {
      setIsCreating(false);
    }
  }, [sessionId, t]);

  const handleCopyNew = useCallback(async () => {
    const url = await createLink();
    if (!url) return;
    await copyText(url);
    toast.show(t("conversation.share.copied"));
  }, [createLink, t, toast]);

  const handleNativeShare = useCallback(async () => {
    const url = await createLink();
    if (!url) return;
    try {
      await navigator.share({ url });
    } catch {
      // the person closed the share sheet
    }
  }, [createLink]);

  const handleCopy = useCallback(
    async (link: ShareLink) => {
      await copyText(absoluteUrl(link.url));
      toast.show(t("conversation.share.copied"));
    },
    [t, toast],
  );

  const handleRevoke = useCallback(async (link: ShareLink) => {
    try {
      await api(`/api/jarvis/shares/${link.id}`, { method: "DELETE" });
      setLinks((current) => (current ?? []).filter((item) => item.id !== link.id));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const header = useMemo<SheetHeader>(() => ({ title: t("conversation.share.title") }), [t]);

  return (
    <AdaptiveModalSheet visible={visible} onClose={onClose} header={header} testID="conversation-share-sheet">
      <View style={styles.body}>
        <Text style={styles.description}>{t("conversation.share.description")}</Text>
        <View style={styles.actions}>
          <Button
            variant="default"
            size="sm"
            style={styles.actionButton}
            onPress={() => void handleCopyNew()}
            disabled={isCreating}
            testID="conversation-share-copy-new"
          >
            {isCreating ? t("conversation.share.creating") : t("conversation.share.copyLink")}
          </Button>
          {canNativeShare ? (
            <Button
              variant="secondary"
              size="sm"
              style={styles.actionButton}
              onPress={() => void handleNativeShare()}
              disabled={isCreating}
            >
              {t("conversation.share.shareLink")}
            </Button>
          ) : null}
        </View>
        {error ? <Text style={styles.error}>{error}</Text> : null}
        {links && links.length > 0 ? (
          <View style={styles.links}>
            <Text style={styles.sectionTitle}>{t("conversation.share.liveLinks")}</Text>
            {links.map((link) => (
              <View key={link.id} style={styles.linkRow}>
                <Text style={styles.linkText} numberOfLines={1}>
                  {new Date(link.createdAt).toLocaleString()}
                </Text>
                <Button variant="ghost" size="sm" onPress={() => void handleCopy(link)}>
                  {t("conversation.share.copy")}
                </Button>
                <Button variant="ghost" size="sm" onPress={() => void handleRevoke(link)}>
                  {t("conversation.share.revoke")}
                </Button>
              </View>
            ))}
          </View>
        ) : null}
      </View>
    </AdaptiveModalSheet>
  );
}

const styles = StyleSheet.create((theme) => ({
  body: {
    gap: theme.spacing[4],
    paddingBottom: theme.spacing[2],
  },
  description: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
  },
  actions: {
    flexDirection: "row",
    gap: theme.spacing[2],
  },
  actionButton: {
    flex: 1,
  },
  error: {
    color: theme.colors.destructive,
    fontSize: theme.fontSize.base,
  },
  links: {
    gap: theme.spacing[1],
  },
  sectionTitle: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  linkRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
  },
  linkText: {
    flex: 1,
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
  },
}));

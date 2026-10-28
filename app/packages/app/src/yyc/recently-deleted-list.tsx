import React, { useCallback, useState } from "react";
import { FlatList, Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { useToast } from "@/contexts/toast-api-context";
import type { AggregatedAgent } from "@/hooks/use-aggregated-agents";
import { getHostRuntimeStore } from "@/runtime/host-runtime";
import { confirmDialog } from "@/utils/confirm-dialog";
import { SIDEBAR_ROW_LINE_HEIGHT } from "@/styles/density";

const RETENTION_DAYS = 30;

function daysLeft(archivedAt: Date | null): number | null {
  if (!archivedAt) return null;
  const purgeAt = archivedAt.getTime() + RETENTION_DAYS * 24 * 60 * 60 * 1000;
  return Math.max(0, Math.ceil((purgeAt - Date.now()) / (24 * 60 * 60 * 1000)));
}

/**
 * 最近刪除 (Recently deleted, docs/paseo-ui.md): what was deleted in the last 30 days, each with 還原 (back to the
 * list) and 永久刪除 (gone for good, its files too). Paseo's History list opens an archived agent instead; here a
 * deleted conversation is only restored or removed.
 */
export function RecentlyDeletedList({
  agents,
  onChanged,
}: {
  agents: AggregatedAgent[];
  onChanged: () => void;
}) {
  return (
    <FlatList
      data={agents}
      keyExtractor={(agent) => `${agent.serverId}:${agent.id}`}
      renderItem={({ item }) => <RecentlyDeletedRow agent={item} onChanged={onChanged} />}
      contentContainerStyle={styles.list}
    />
  );
}

function RecentlyDeletedRow({ agent, onChanged }: { agent: AggregatedAgent; onChanged: () => void }) {
  const { t } = useTranslation();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const left = daysLeft(agent.archivedAt ?? null);

  const run = useCallback(
    async (action: (client: NonNullable<ReturnType<ReturnType<typeof getHostRuntimeStore>["getClient"]>>) => Promise<unknown>) => {
      const client = getHostRuntimeStore().getClient(agent.serverId);
      if (!client) return;
      setBusy(true);
      try {
        await action(client);
        onChanged();
      } catch (error) {
        toast.error(error instanceof Error ? error.message : String(error));
      } finally {
        setBusy(false);
      }
    },
    [agent.serverId, onChanged, toast],
  );

  const restore = useCallback(() => run((client) => client.refreshAgent(agent.id)), [agent.id, run]);
  const deleteForGood = useCallback(async () => {
    const confirmed = await confirmDialog({
      title: t("conversation.deleted.confirmTitle"),
      message: t("conversation.deleted.confirmMessage", { title: agent.title ?? "" }),
      confirmLabel: t("conversation.deleted.deleteForGood"),
      cancelLabel: t("common.actions.cancel"),
      destructive: true,
    });
    if (confirmed) await run((client) => client.deleteAgent(agent.id));
  }, [agent.id, agent.title, run, t]);

  return (
    <View style={styles.row} testID="recently-deleted-row">
      <View style={styles.text}>
        <Text style={styles.title} numberOfLines={1}>
          {agent.title ?? t("agentList.fallbackTitle")}
        </Text>
        {left !== null ? (
          <Text style={styles.meta}>{t("conversation.deleted.daysLeft", { count: left })}</Text>
        ) : null}
      </View>
      <Button variant="secondary" size="sm" onPress={() => void restore()} disabled={busy}>
        {t("conversation.deleted.restore")}
      </Button>
      <Button variant="ghost" size="sm" onPress={() => void deleteForGood()} disabled={busy}>
        {t("conversation.deleted.deleteForGood")}
      </Button>
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  list: {
    width: "100%",
    // The chat column's width, which follows the content-width setting.
    maxWidth: theme.contentMaxWidth,
    alignSelf: "center",
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[4],
  },
  row: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingVertical: theme.spacing[3],
    borderBottomWidth: 1,
    borderBottomColor: theme.colors.border,
  },
  text: {
    flex: 1,
    minWidth: 0,
  },
  title: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    lineHeight: SIDEBAR_ROW_LINE_HEIGHT,
  },
  meta: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    lineHeight: 16,
  },
}));

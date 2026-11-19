import { useCallback } from "react";
import { Pressable, Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { ArrowUp, Pencil, X } from "lucide-react-native";
import { lazyArea } from "@/components/lazy-area";
import type { QueuedComposerMessage } from "@/composer/actions";
import { ICON_SIZE, type Theme } from "@/styles/theme";
import { loadReplyChunk } from "@/loading/stages";
import { PENDING_BUBBLE_OPACITY } from "@/yyc/outbox";
import { cancelOutboxEntry } from "@/yyc/outbox-store";
import type { ServerQueueItem } from "@/yyc/server-queue";

/**
 * The rows of the queue above the message box (the server's queue, and the app's own where the server keeps none).
 * Their own chunk (composer/index.tsx QueueRows): a queue shows only after a message is sent during a reply, so they
 * stay out of the first load (docs/paseo-ui.md "Budgets").
 */

// The line under a queued message the server has not confirmed lives in the reply chunk, which has arrived before
// anything can be sent (loading/stages.ts), so it draws offline too.
const PendingSendLine = lazyArea(
  "pending-send",
  loadReplyChunk,
  (module) => module.PendingSendLine,
  null,
);

export interface ServerQueuedRowProps {
  item: ServerQueueItem;
  onTakeBack: (id: string) => void;
  onSendNow: () => void;
  takeBackLabel: string;
  sendNowLabel: string;
  cancelLabel: string;
}

/**
 * A message of the server's queue: its text, "send now" (the reply stops, so the queue's first message runs), and
 * for one the server holds until the reply ends, taking it back.
 */
export function ServerQueuedRow({
  item,
  onTakeBack,
  onSendNow,
  takeBackLabel,
  sendNowLabel,
  cancelLabel,
}: ServerQueuedRowProps) {
  const handleTakeBack = useCallback(() => {
    onTakeBack(item.id);
  }, [onTakeBack, item.id]);
  const handleCancelPending = useCallback(() => {
    cancelOutboxEntry(item.id);
  }, [item.id]);
  if (item.pendingSinceMs !== undefined) {
    // the server has not confirmed it (yyc/outbox.ts): faded, its line under the text, and 取消 takes it back
    return (
      <View style={styles.queueItem} testID="server-queue-item" aria-busy>
        <View style={styles.queueBody}>
          <Text style={[styles.queueText, styles.queueTextPending]} numberOfLines={2} ellipsizeMode="tail">
            {item.text}
          </Text>
          <PendingSendLine sinceMs={item.pendingSinceMs} compact />
        </View>
        <View style={styles.queueActions}>
          <Pressable
            onPress={handleCancelPending}
            style={styles.queueActionButton}
            accessibilityLabel={cancelLabel}
            accessibilityRole="button"
            testID="server-queue-cancel-pending"
          >
            <ThemedX size={ICON_SIZE.sm} uniProps={iconForegroundMapping} />
          </Pressable>
        </View>
      </View>
    );
  }
  return (
    <View style={styles.queueItem} testID="server-queue-item">
      <Text style={styles.queueText} numberOfLines={2} ellipsizeMode="tail">
        {item.text}
      </Text>
      <View style={styles.queueActions}>
        {item.held ? (
          <Pressable
            onPress={handleTakeBack}
            style={styles.queueActionButton}
            accessibilityLabel={takeBackLabel}
            accessibilityRole="button"
            testID="server-queue-take-back"
          >
            <ThemedPencil size={ICON_SIZE.sm} uniProps={iconForegroundMapping} />
          </Pressable>
        ) : null}
        <Pressable
          onPress={onSendNow}
          style={[styles.queueActionButton, styles.queueSendButton]}
          accessibilityLabel={sendNowLabel}
          accessibilityRole="button"
          testID="server-queue-send-now"
        >
          <ThemedArrowUp size={ICON_SIZE.sm} uniProps={iconAccentForegroundMapping} />
        </Pressable>
      </View>
    </View>
  );
}

export interface QueuedMessageRowProps {
  item: QueuedComposerMessage;
  onEdit: (id: string) => void;
  onSendNow: (id: string) => void;
  editLabel: string;
  sendNowLabel: string;
}

export function QueuedMessageRow({
  item,
  onEdit,
  onSendNow,
  editLabel,
  sendNowLabel,
}: QueuedMessageRowProps) {
  const handleEdit = useCallback(() => {
    onEdit(item.id);
  }, [onEdit, item.id]);
  const handleSendNow = useCallback(() => {
    onSendNow(item.id);
  }, [onSendNow, item.id]);
  return (
    <View style={styles.queueItem}>
      <Text style={styles.queueText} numberOfLines={2} ellipsizeMode="tail">
        {item.text}
      </Text>
      <View style={styles.queueActions}>
        <Pressable
          onPress={handleEdit}
          style={styles.queueActionButton}
          accessibilityLabel={editLabel}
          accessibilityRole="button"
        >
          <ThemedPencil size={ICON_SIZE.sm} uniProps={iconForegroundMapping} />
        </Pressable>
        <Pressable
          onPress={handleSendNow}
          style={[styles.queueActionButton, styles.queueSendButton]}
          accessibilityLabel={sendNowLabel}
          accessibilityRole="button"
        >
          <ThemedArrowUp size={ICON_SIZE.sm} uniProps={iconAccentForegroundMapping} />
        </Pressable>
      </View>
    </View>
  );
}

const ThemedPencil = withUnistyles(Pencil);
const ThemedArrowUp = withUnistyles(ArrowUp);
const ThemedX = withUnistyles(X);
const iconForegroundMapping = (theme: Theme) => ({ color: theme.colors.foreground });
const iconAccentForegroundMapping = (theme: Theme) => ({ color: theme.colors.accentForeground });

const styles = StyleSheet.create((theme: Theme) => ({
  queueItem: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    paddingHorizontal: theme.spacing[3],
    paddingVertical: theme.spacing[2],
    backgroundColor: theme.colors.surface1,
    borderRadius: theme.borderRadius.lg,
    borderWidth: theme.borderWidth[1],
    borderColor: theme.colors.border,
    gap: theme.spacing[2],
  },
  queueBody: {
    flex: 1,
    minWidth: 0,
  },
  queueText: {
    flex: 1,
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
  },
  queueTextPending: {
    opacity: PENDING_BUBBLE_OPACITY,
  },
  queueActions: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
  },
  queueActionButton: {
    width: 32,
    height: 32,
    borderRadius: theme.borderRadius.full,
    alignItems: "center",
    justifyContent: "center",
    backgroundColor: theme.colors.surface2,
  },
  queueSendButton: {
    backgroundColor: theme.colors.accent,
  },
}));

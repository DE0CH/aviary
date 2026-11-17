import { Clock, WifiOff } from "lucide-react-native";
import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Pressable, Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import type { Theme } from "@/styles/theme";
import { PENDING_WAIT_MS, pendingPhase } from "./outbox";

export { PENDING_BUBBLE_OPACITY } from "./outbox";

/**
 * The line under a message the server has not confirmed (design A, Deyao 2026-11-17): a clock and 「傳送中」, and after
 * `PENDING_WAIT_MS` 「等待網絡，連線後自動傳送」 with 取消 when the message can still be taken back. It stands where the
 * time stands once the message is confirmed, at the same height, so nothing moves when the confirmation comes.
 */
export function PendingSendLine({
  sinceMs,
  onCancel,
  compact = false,
}: {
  sinceMs: number;
  /** Absent: the message is on its way and can no longer be taken back. */
  onCancel?: () => void;
  /** In the queue above the message box: under the text, flush left. */
  compact?: boolean;
}) {
  const { t } = useTranslation();
  const [nowMs, setNowMs] = useState(() => Date.now());
  const phase = pendingPhase(sinceMs, nowMs);
  useEffect(() => {
    if (phase === "waiting") return undefined;
    // once: when it has waited long enough to say so
    const timer = setTimeout(() => setNowMs(Date.now()), Math.max(0, sinceMs + PENDING_WAIT_MS - Date.now()));
    return () => clearTimeout(timer);
  }, [phase, sinceMs]);
  const Icon = phase === "waiting" ? ThemedWifiOff : ThemedClock;
  return (
    <View
      style={[styles.line, compact ? styles.compact : null]}
      testID="pending-send-line"
      accessibilityLiveRegion="polite"
    >
      <Icon size={13} uniProps={mutedMapping} />
      <Text style={styles.text} testID="pending-send-text">
        {phase === "waiting" ? t("message.pending.waiting") : t("message.pending.sending")}
      </Text>
      {phase === "waiting" && onCancel ? (
        <Pressable
          onPress={onCancel}
          accessibilityRole="button"
          hitSlop={8}
          testID="pending-send-cancel"
        >
          <Text style={styles.cancel}>{t("message.pending.cancel")}</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

const ThemedClock = withUnistyles(Clock);
const ThemedWifiOff = withUnistyles(WifiOff);
const mutedMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

const styles = StyleSheet.create((theme) => ({
  line: {
    alignSelf: "flex-end",
    flexDirection: "row",
    alignItems: "center",
    height: 24,
    gap: theme.spacing[1],
    marginTop: theme.spacing[2],
  },
  compact: {
    alignSelf: "flex-start",
    height: 18,
    marginTop: 2,
  },
  text: {
    color: theme.colors.foregroundMuted,
    fontSize: 13,
  },
  cancel: {
    color: theme.colors.accent,
    fontSize: 13,
    fontWeight: theme.fontWeight.semibold,
    marginLeft: theme.spacing[2],
  },
}));

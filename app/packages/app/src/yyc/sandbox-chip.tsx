import React, { useEffect, useState } from "react";
import { Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { LoadingSpinner } from "@/components/ui/loading-spinner";
import { useSandboxPreparingSince } from "@/yyc/sandbox-status";
import type { Theme } from "@/styles/theme";

const ThemedLoadingSpinner = withUnistyles(LoadingSpinner);
const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });

const SLOW_AFTER_MS = 20_000;

/**
 * 「正在準備工作環境…」 above the composer while the conversation's sandbox is being made (a host may be booting,
 * which takes a minute or two); after 20 s it says how long this can take (docs/paseo-ui.md).
 */
export function SandboxPreparingChip({ serverId, agentId }: { serverId: string; agentId: string }) {
  const { t } = useTranslation();
  const since = useSandboxPreparingSince(serverId, agentId);
  const [slow, setSlow] = useState(false);

  useEffect(() => {
    if (since === null) {
      setSlow(false);
      return;
    }
    const remaining = SLOW_AFTER_MS - (Date.now() - since);
    if (remaining <= 0) {
      setSlow(true);
      return;
    }
    const timer = setTimeout(() => setSlow(true), remaining);
    return () => clearTimeout(timer);
  }, [since]);

  if (since === null) return null;
  return (
    <View style={styles.row} testID="sandbox-preparing" accessibilityRole="progressbar">
      <View style={styles.chip}>
        <ThemedLoadingSpinner size="small" uniProps={mutedColorMapping} />
        <Text style={styles.text}>{t("conversation.preparing.title")}</Text>
      </View>
      {slow ? <Text style={styles.hint}>{t("conversation.preparing.slow")}</Text> : null}
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  row: {
    alignItems: "center",
    gap: theme.spacing[1],
    paddingBottom: theme.spacing[2],
  },
  chip: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingHorizontal: theme.spacing[3],
    paddingVertical: theme.spacing[1],
    borderRadius: theme.borderRadius.full,
    backgroundColor: theme.colors.surface2,
  },
  text: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  hint: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
}));

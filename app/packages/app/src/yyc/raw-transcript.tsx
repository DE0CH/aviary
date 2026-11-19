/**
 * The admin's "Raw transcript" chip (Deyao, 2026-11-19: the approved mock A): in a view-as tab (yyc/view-as.ts) on a
 * conversation, the last chip of the header's right group, after the activity and 排程 chips, opens pigeon's raw view
 * (`/raw/<conversation id>?back=<this page>`, pigeon raw/). It replaces pigeon's injected fixed button, which covered
 * those chips. English and the view-as box's dashed amber edge, as all the admin-only UI (view-as-box.tsx). On a phone
 * the icon alone: the words leave the title no room.
 */
import React from "react";
import { Pressable, Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { ScrollText } from "lucide-react-native";
import { TOUCH_TARGET } from "@/components/ui/control-geometry";
import { isWeb } from "@/constants/platform";
import { useViewAsStore } from "@/yyc/view-as";
import type { Theme } from "@/styles/theme";

/** pigeon's raw view of a conversation, coming back to `here` (the page's path and query). */
export function rawTranscriptHref(sessionId: string, here: string): string {
  return `/raw/${encodeURIComponent(sessionId)}?back=${encodeURIComponent(here)}`;
}

const mutedColor = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const ThemedScroll = withUnistyles(ScrollText);

export function RawTranscriptChip({ sessionId }: { sessionId: string | null }) {
  const viewAs = useViewAsStore((s) => s.viewAs);
  if (!isWeb || !viewAs || !sessionId) return null;
  return (
    <Pressable
      testID="raw-transcript-chip"
      accessibilityRole="link"
      accessibilityLabel="Raw transcript (visible only to you)"
      onPress={() =>
        window.location.assign(rawTranscriptHref(sessionId, window.location.pathname + window.location.search))
      }
      style={styles.chipTarget}
    >
      <View style={styles.chip}>
        <ThemedScroll size={13} uniProps={mutedColor} />
        <Text style={styles.chipLabel}>Raw transcript</Text>
      </View>
    </Pressable>
  );
}

const styles = StyleSheet.create((theme) => ({
  // a finger's 44 px on a phone, where the chip is the icon alone
  chipTarget: {
    minHeight: { xs: TOUCH_TARGET, md: 0 },
    minWidth: { xs: TOUCH_TARGET, md: 0 },
    justifyContent: "center",
    alignItems: "center",
    marginVertical: { xs: -10, md: 0 },
  },
  chip: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
    paddingHorizontal: theme.spacing[2],
    paddingVertical: { xs: theme.spacing[1], md: theme.spacing[0.5] },
    borderRadius: theme.borderRadius.full,
    borderWidth: 1,
    borderStyle: "dashed",
    borderColor: theme.colors.statusWarning,
  },
  // a phone's header has no room for the words beside the title and the chips: the icon alone there
  chipLabel: {
    display: { xs: "none", md: "flex" },
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
}));

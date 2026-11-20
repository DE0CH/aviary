/**
 * The admin's "Raw transcript" chip (Deyao, 2026-11-19: the approved mock A): in a view-as tab (yyc/view-as.ts) on a
 * conversation, the last chip of the header's right group, after the activity and 排程 chips, opens pigeon's raw view
 * (`/raw/<conversation id>?back=<this page>`, pigeon raw/). It replaces pigeon's injected fixed button, which covered
 * those chips. English and the view-as box's dashed amber edge, as all the admin-only UI (view-as-box.tsx). On a phone
 * the icon alone: the words leave the title no room.
 */
import React from "react";
import { withUnistyles } from "react-native-unistyles";
import { ScrollText } from "lucide-react-native";
import { HeaderChip } from "@/yyc/header-chip";
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
    <HeaderChip
      testID="raw-transcript-chip"
      accessibilityRole="link"
      accessibilityLabel="Raw transcript (visible only to you)"
      icon={<ThemedScroll size={13} uniProps={mutedColor} />}
      label="Raw transcript"
      tone="admin"
      labelOnPhone={false}
      onPress={() =>
        window.location.assign(rawTranscriptHref(sessionId, window.location.pathname + window.location.search))
      }
    />
  );
}

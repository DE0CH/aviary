/**
 * A conversation's side panel (Activity, Schedules) is content of upstream's right-hand panel frame
 * (`screens/workspace/explorer-sidebar/frame.tsx`, mounted by `conversation-side-panel.tsx`): docked beside the chat on
 * a wide screen, sliding over it on a phone, and keeping its column clear of the status bar in both. A panel here has
 * no layout or safe area of its own: its header is the app's (`CompactHeader` floating behind the chat's fade on a
 * phone, `ScreenHeader` beside the chat). Deyao, 2026-11-20: a shell of our own left room for the status bar only on a
 * phone, and the docked Activity on the unfolded Fold sat under it.
 */
import React from "react";
import type { ReactNode } from "react";
import { ScrollView, Text } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { X } from "lucide-react-native";
import { create } from "zustand";
import { CompactHeader } from "@/components/headers/compact-header";
import { ScreenHeader } from "@/components/headers/screen-header";
import { FloatingHeaderLayout } from "@/components/floating-header";
import { HeaderToggleButton } from "@/components/headers/header-toggle-button";
import { mutedLargeGlyphProps } from "@/components/ui/icon-button-chrome";
import { useIsCompactFormFactor } from "@/constants/layout";
import { usePanelStore } from "@/stores/panel-store";
import { sidePanelOpen, type SidePanelKind } from "./side-panel-state";

export type { SidePanelKind } from "./side-panel-state";

const ThemedX = withUnistyles(X);
const NO_SHORTCUT: never[] = [];

/** The panel chosen for a conversation; the frame shows it (on a phone while its overlay is out). */
export const useSidePanel = create<{
  shown: { sessionId: string; kind: SidePanelKind } | null;
}>(() => ({
  shown: null,
}));

/** Whether this conversation's panel of this kind is on screen, and what a tap on its chip does. */
export function useSidePanelToggle(
  sessionId: string | null,
  kind: SidePanelKind
): {
  open: boolean;
  toggle: () => void;
} {
  const compact = useIsCompactFormFactor();
  const shown = useSidePanel((s) => s.shown);
  const overlayOut = usePanelStore(
    (s) => s.mobilePanel.target === "file-explorer"
  );
  const open = sidePanelOpen({ shown, sessionId, kind, compact, overlayOut });
  const toggle = () => {
    if (!sessionId) return;
    if (open) {
      closeSidePanel();
      return;
    }
    useSidePanel.setState({ shown: { sessionId, kind } });
    if (compact) usePanelStore.getState().showMobileFileExplorer();
  };
  return { open, toggle };
}

export function closeSidePanel(): void {
  useSidePanel.setState({ shown: null });
  if (usePanelStore.getState().mobilePanel.target === "file-explorer")
    usePanelStore.getState().showMobileAgent();
}

/** A side panel's page: the app's header, then its scrolling body. `<id>-panel` and `<id>-title` are its test IDs. */
export function SidePanelPage({
  id,
  title,
  compact,
  closeLabel,
  children,
}: {
  id: string;
  title: string;
  compact: boolean;
  closeLabel: string;
  children: ReactNode;
}) {
  const close = closeSidePanel;
  const header = compact ? (
    <CompactHeader
      framed
      navigation="back"
      onBack={close}
      title={title}
      titleTestID={`${id}-title`}
    />
  ) : (
    <ScreenHeader
      framed
      left={
        <Text style={styles.title} numberOfLines={1} testID={`${id}-title`}>
          {title}
        </Text>
      }
      right={
        <HeaderToggleButton
          onPress={close}
          tooltipLabel={closeLabel}
          tooltipKeys={NO_SHORTCUT}
          tooltipSide="left"
          testID={`${id}-close`}
          accessibilityRole="button"
          accessibilityLabel={closeLabel}
        >
          <ThemedX strokeWidth={1.5} uniProps={mutedLargeGlyphProps} />
        </HeaderToggleButton>
      }
    />
  );
  return (
    <FloatingHeaderLayout header={header} floating={compact}>
      {(topOverlayInset) => (
        <ScrollView
          testID={`${id}-panel`}
          style={styles.body}
          contentContainerStyle={[
            styles.content,
            { paddingTop: topOverlayInset },
          ]}
        >
          {children}
        </ScrollView>
      )}
    </FloatingHeaderLayout>
  );
}

const styles = StyleSheet.create((theme) => ({
  title: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.semibold,
  },
  body: {
    flex: 1,
  },
  content: {
    paddingBottom: theme.spacing[6],
  },
}));

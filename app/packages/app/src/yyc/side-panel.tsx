/**
 * The shell of a conversation's side panel (Activity, Schedules): beside the chat on a wide screen, over it on a phone.
 * On a phone the panel covers the whole screen, so the shell keeps its header below the status bar and its last line
 * above the home indicator: a panel built on it never handles the safe area itself.
 */
import React from "react";
import type { ReactNode } from "react";
import { Pressable, ScrollView, Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { ChevronLeft, X } from "lucide-react-native";
import type { Theme } from "@/styles/theme";

const ThemedX = withUnistyles(X);
const ThemedBack = withUnistyles(ChevronLeft);
const mutedColor = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const foregroundColor = (theme: Theme) => ({ color: theme.colors.foreground });

export function SidePanel({
  id,
  title,
  compact,
  onClose,
  backLabel,
  closeLabel,
  children,
}: {
  /** `<id>-panel`, `<id>-back` and `<id>-close` are the test IDs. */
  id: string;
  title: string;
  /** A phone: the panel covers the chat and closes with a back arrow. */
  compact: boolean;
  onClose: () => void;
  backLabel: string;
  closeLabel: string;
  /** The scrolling body. */
  children: ReactNode;
}) {
  const insets = useSafeAreaInsets();
  return (
    <View
      style={
        compact
          ? [styles.panelPhone, { paddingTop: insets.top, paddingLeft: insets.left, paddingRight: insets.right }]
          : styles.panelDock
      }
      testID={`${id}-panel`}
    >
      <View style={styles.panelHeader}>
        {compact ? (
          <Pressable
            onPress={onClose}
            accessibilityRole="button"
            accessibilityLabel={backLabel}
            style={styles.iconButton}
            testID={`${id}-back`}
          >
            <ThemedBack size={20} uniProps={foregroundColor} />
          </Pressable>
        ) : null}
        <Text style={styles.panelTitle}>{title}</Text>
        {compact ? null : (
          <Pressable
            onPress={onClose}
            accessibilityRole="button"
            accessibilityLabel={closeLabel}
            style={styles.iconButton}
            testID={`${id}-close`}
          >
            <ThemedX size={16} uniProps={mutedColor} />
          </Pressable>
        )}
      </View>
      <ScrollView style={styles.panelBody} contentContainerStyle={styles.panelContent}>
        {children}
        {compact ? <View style={{ height: insets.bottom }} testID={`${id}-home-indicator`} /> : null}
      </ScrollView>
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  panelDock: {
    width: 340,
    flexShrink: 0,
    borderLeftWidth: 1,
    borderLeftColor: theme.colors.border,
    backgroundColor: theme.colors.surface0,
    minHeight: 0,
  },
  panelPhone: {
    position: "absolute",
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    zIndex: 50,
    backgroundColor: theme.colors.surface0,
  },
  panelHeader: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingHorizontal: theme.spacing[3],
    // the 44 px back button sets the height on a phone
    paddingVertical: { xs: theme.spacing[1], md: theme.spacing[3] },
    borderBottomWidth: 1,
    borderBottomColor: theme.colors.border,
  },
  panelTitle: {
    flex: 1,
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.semibold,
  },
  // a finger's 44 px on a phone (the 20 px arrow + 12 px each side), the glyph where it was
  iconButton: {
    padding: { xs: 12, md: theme.spacing[1] },
    marginHorizontal: { xs: -8, md: 0 },
  },
  panelBody: {
    flex: 1,
  },
  panelContent: {
    paddingBottom: theme.spacing[6],
  },
}));

import { useCallback, useState, type ComponentType, type ReactNode, type Ref } from "react";
import { Pressable, Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { HEADER_INNER_HEIGHT, HEADER_INNER_HEIGHT_MOBILE } from "@/constants/layout";
import { DensityIcon } from "@/components/ui/density-icon";
import { Shortcut, SHOW_SHORTCUT_HINTS } from "@/components/ui/shortcut";
import { density, densityIconBox, SIDEBAR_ROW_LINE_HEIGHT } from "@/styles/density";
import type { ShortcutKey } from "@/utils/format-shortcut";

type SidebarHeaderRowVariant = "header" | "compact" | "inline";

export type SidebarRowIcon = ComponentType<{ size: number; color: string }>;

interface SidebarHeaderRowProps {
  icon: SidebarRowIcon | null;
  label: string;
  onPress: () => void;
  isActive?: boolean;
  testID?: string;
  nativeID?: string;
  accessibilityLabel?: string;
  /**
   * "header" (default): a sidebar-height row with its own bottom separator —
   * the lone header at the top of a sidebar (settings "Back to workspace").
   * "compact": a sidebar list row with no separator (the items at the top of the sidebar, the settings list): the
   * same height, inset and label as a conversation row.
   * "inline": a full-width row with no inset, for a row inside a padded container such as the
   * sidebar footer.
   */
  variant?: SidebarHeaderRowVariant;
  /** Shown in the right slot while the row is hovered, when `trailing` is not set. */
  shortcutKeys?: ShortcutKey[][] | null;
  /**
   * The right slot. A sibling of the row's button, never inside it (web cannot nest buttons). A
   * press on the slot presses the row; a button inside it presses on its own.
   */
  trailing?: ReactNode;
  rowRef?: Ref<View>;
}

export function SidebarHeaderRow({
  icon,
  label,
  onPress,
  isActive = false,
  testID,
  nativeID,
  accessibilityLabel,
  variant = "header",
  shortcutKeys = null,
  trailing,
  rowRef,
}: SidebarHeaderRowProps) {
  const [isHovered, setIsHovered] = useState(false);
  const handlePointerEnter = useCallback(() => setIsHovered(true), []);
  const handlePointerLeave = useCallback(() => setIsHovered(false), []);
  // The button and its trailing slot are siblings, so the row owns the pressed fill for both.
  const [isPressed, setIsPressed] = useState(false);
  const handlePressIn = useCallback(() => setIsPressed(true), []);
  const handlePressOut = useCallback(() => setIsPressed(false), []);
  const isHighlighted = isHovered || isActive;

  const isListRow = variant === "compact";

  let right = trailing ?? null;
  // No hint slot at all while hints are off: an empty trailing slot would still take 8 px on hover.
  if (right === null && SHOW_SHORTCUT_HINTS && shortcutKeys && isHovered) {
    right = <Shortcut chord={shortcutKeys} />;
  }

  return (
    <View ref={rowRef} collapsable={false} style={getContainerStyle(variant)}>
      <View
        style={[
          styles.row,
          isListRow && styles.rowList,
          isHighlighted && styles.rowHighlighted,
          isActive && styles.rowActive,
          isPressed && styles.rowPressed,
        ]}
        onPointerEnter={handlePointerEnter}
        onPointerLeave={handlePointerLeave}
      >
        <Pressable
          onPress={onPress}
          onPressIn={handlePressIn}
          onPressOut={handlePressOut}
          testID={testID}
          nativeID={nativeID}
          accessible
          accessibilityRole="button"
          accessibilityLabel={accessibilityLabel ?? label}
          accessibilityState={isActive ? SELECTED_STATE : undefined}
          aria-selected={isActive}
          style={[styles.button, isListRow && styles.buttonList]}
        >
          {/* An sm glyph in an md slot: the slot is a conversation row's status slot, so labels share one rail. */}
          <View style={styles.iconSlot}>
            {icon ? (
              <DensityIcon
                icon={icon}
                size="sm"
                color={isHighlighted ? "foreground" : "foregroundMuted"}
              />
            ) : null}
          </View>
          <Text
            style={[
              styles.label,
              isListRow && styles.labelList,
              isHighlighted && styles.labelHighlighted,
            ]}
          >
            {label}
          </Text>
        </Pressable>
        {right === null ? null : (
          <Pressable
            onPress={onPress}
            onPressIn={handlePressIn}
            onPressOut={handlePressOut}
            accessible={false}
            focusable={false}
            style={styles.trailing}
          >
            {right}
          </Pressable>
        )}
      </View>
    </View>
  );
}

const SELECTED_STATE = { selected: true } as const;

const styles = StyleSheet.create((theme) => ({
  container: {
    height: {
      xs: HEADER_INNER_HEIGHT_MOBILE,
      md: HEADER_INNER_HEIGHT,
    },
    paddingHorizontal: theme.spacing[2],
    justifyContent: "center",
    borderBottomWidth: 1,
    borderBottomColor: theme.colors.border,
    userSelect: "none",
  },
  containerCompact: {
    paddingHorizontal: theme.spacing[2],
    justifyContent: "center",
    userSelect: "none",
  },
  containerInline: {
    width: "100%",
    justifyContent: "center",
    userSelect: "none",
  },
  row: {
    flexDirection: "row",
    alignItems: "center",
    // Shorter than the header strip so the hover highlight clears the strip's bottom separator.
    minHeight: density.tightRowHeight,
    borderRadius: theme.borderRadius.lg,
  },
  // A sidebar list row: the conversation rows' geometry (sidebar-workspace-list.tsx workspaceRow).
  rowList: {
    minHeight: density.rowHeight,
  },
  rowHighlighted: {
    backgroundColor: theme.colors.surfaceSidebarHover,
  },
  // A selected conversation row's fill.
  rowActive: {
    backgroundColor: theme.colors.surfaceSidebarSelected,
  },
  // The workspace rows' pressed fill.
  rowPressed: {
    backgroundColor: theme.colors.surface2,
  },
  button: {
    flex: 1,
    minWidth: 0,
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    minHeight: density.tightRowHeight,
    paddingVertical: theme.spacing[1],
    // The conversation rows' inner padding, so the icons align on one vertical edge with the list below.
    paddingHorizontal: theme.spacing[2],
  },
  buttonList: {
    minHeight: density.rowHeight,
    paddingVertical: theme.spacing[2],
  },
  iconSlot: {
    width: densityIconBox("md"),
    height: densityIconBox("md"),
    alignItems: "center",
    justifyContent: "center",
    flexShrink: 0,
  },
  label: {
    flexShrink: 1,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.normal,
    color: theme.colors.foregroundMuted,
  },
  labelList: {
    lineHeight: SIDEBAR_ROW_LINE_HEIGHT,
  },
  labelHighlighted: {
    color: theme.colors.foreground,
  },
  trailing: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
    paddingRight: theme.spacing[2],
  },
}));

function getContainerStyle(variant: SidebarHeaderRowVariant) {
  switch (variant) {
    case "header":
      return styles.container;
    case "compact":
      return styles.containerCompact;
    case "inline":
      return styles.containerInline;
  }
}

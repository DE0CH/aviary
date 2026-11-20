/**
 * A chip in the conversation's header (Activity 「n running」, Schedules 「n 個排程」, the admin's Raw transcript): a
 * pill with an icon and a label that opens something. One piece for all of them, which shows a finger on it the way
 * upstream's `Button` does, from the press itself (Deyao, 2026-11-20: the chips were three copies on a bare Pressable,
 * and a touch showed nothing). The pressable is a finger's 44 px on a phone around the drawn pill, which keeps the
 * header's height (docs/paseo-ui.md "Touch targets").
 */
import React from "react";
import type { ReactNode } from "react";
import { Pressable, Text, View } from "react-native";
import type { AccessibilityRole } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { TOUCH_TARGET } from "@/components/ui/control-geometry";

export function HeaderChip({
  icon,
  label,
  open = false,
  tone = "plain",
  labelOnPhone = true,
  onPress,
  testID,
  accessibilityRole = "button",
  accessibilityLabel,
}: {
  icon: ReactNode;
  label: string;
  /** What it opens is open: the pill is filled. */
  open?: boolean;
  /** `admin`: a dashed warning edge (something only the admin sees). */
  tone?: "plain" | "admin";
  /** false: the icon alone on a phone, whose header has no room for the words. */
  labelOnPhone?: boolean;
  onPress: () => void;
  testID: string;
  accessibilityRole?: AccessibilityRole;
  accessibilityLabel?: string;
}) {
  return (
    <Pressable
      testID={testID}
      accessibilityRole={accessibilityRole}
      accessibilityState={
        accessibilityRole === "button" ? { expanded: open } : undefined
      }
      accessibilityLabel={accessibilityLabel ?? label}
      onPress={onPress}
      style={
        labelOnPhone ? styles.target : [styles.target, styles.targetIconOnly]
      }
    >
      {({ pressed }) => (
        <View
          style={[
            styles.chip,
            open && styles.chipOpen,
            tone === "admin" && styles.admin,
            pressed && styles.pressed,
          ]}
        >
          {icon}
          <Text
            style={
              labelOnPhone ? styles.label : [styles.label, styles.labelWideOnly]
            }
          >
            {label}
          </Text>
        </View>
      )}
    </Pressable>
  );
}

const styles = StyleSheet.create((theme) => ({
  // a finger's 44 px tall on a phone around the drawn pill, the pill itself from md up
  target: {
    minHeight: { xs: TOUCH_TARGET, md: 0 },
    justifyContent: "center",
    alignItems: "center",
    marginVertical: { xs: -10, md: 0 },
  },
  // the icon alone on a phone: 44 px wide too
  targetIconOnly: {
    minWidth: { xs: TOUCH_TARGET, md: 0 },
  },
  chip: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
    paddingHorizontal: theme.spacing[2],
    paddingVertical: theme.spacing[0.5],
    borderRadius: theme.borderRadius.full,
    borderWidth: 1,
    borderColor: theme.colors.border,
    backgroundColor: theme.colors.surface1,
  },
  chipOpen: {
    backgroundColor: theme.colors.surface3,
  },
  admin: {
    borderStyle: "dashed",
    borderColor: theme.colors.statusWarning,
    backgroundColor: "transparent",
    paddingVertical: { xs: theme.spacing[1], md: theme.spacing[0.5] },
  },
  pressed: {
    backgroundColor: theme.colors.surface3,
  },
  label: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  labelWideOnly: {
    display: { xs: "none", md: "flex" },
  },
}));

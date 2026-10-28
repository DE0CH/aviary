import React from "react";
import { Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { Switch } from "@/components/ui/switch";
import { useViewAsStore } from "@/yyc/view-as";

/**
 * View as (yyc/view-as.ts): the one box only the admin sees, above the composer of a view-as tab. English (Deyao's
 * rule for what only he sees, YYC CLAUDE.md). It says whose view this is and that it is read-only, and holds the
 * override that lets the admin send messages and start conversations AS that person (off on every page load). With
 * the override on, the box turns red so it never reads as the normal state. Sends are recorded in Ops History by the
 * server; the person's messages look the same either way.
 */
export function ViewAsBox() {
  const viewAs = useViewAsStore((state) => state.viewAs);
  const override = useViewAsStore((state) => state.override);
  const setOverride = useViewAsStore((state) => state.setOverride);
  if (!viewAs) return null;
  const expired = viewAs.expiresAt <= Date.now();
  return (
    <View
      style={[styles.box, override ? styles.boxOverride : null]}
      testID="view-as-box"
      accessibilityLabel="Visible only to you"
    >
      <Text style={styles.label}>Visible only to you (admin)</Text>
      <Text style={styles.title}>
        Viewing as <Text style={styles.strong}>{viewAs.user}</Text>
        {override ? " — override ON" : " — read-only"}
      </Text>
      {expired ? (
        <Text style={styles.text}>This view has expired. Open View as again from the Ops page.</Text>
      ) : null}
      <View style={styles.row}>
        <Switch
          value={override}
          onValueChange={setOverride}
          disabled={expired}
          accessibilityLabel={`Override read-only: send as ${viewAs.user}`}
          testID="view-as-override"
        />
        <Text style={styles.text}>
          {override
            ? `Override: messages and new conversations you send now go out as ${viewAs.user}, look exactly like theirs, and are logged in Ops History.`
            : `Override read-only: let me send messages and start conversations as ${viewAs.user}.`}
        </Text>
      </View>
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  box: {
    gap: theme.spacing[1],
    marginBottom: theme.spacing[2],
    paddingHorizontal: theme.spacing[3],
    paddingVertical: theme.spacing[2],
    borderRadius: theme.borderRadius.lg,
    borderWidth: 1,
    borderStyle: "dashed",
    borderColor: theme.colors.statusWarning,
    backgroundColor: theme.colors.statusWarningTint,
  },
  boxOverride: {
    borderStyle: "solid",
    borderColor: theme.colors.statusDanger,
    backgroundColor: theme.colors.statusDangerTint,
  },
  label: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    fontWeight: "600",
    textTransform: "uppercase",
    letterSpacing: 0.4,
  },
  title: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  strong: {
    fontWeight: "600",
  },
  row: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
  },
  text: {
    flex: 1,
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
}));

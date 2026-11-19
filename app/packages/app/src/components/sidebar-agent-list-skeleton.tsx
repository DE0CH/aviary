import { useEffect, useMemo, useRef } from "react";
import { Animated, View, type StyleProp, type ViewStyle } from "react-native";
import { useTranslation } from "react-i18next";
import { StyleSheet } from "react-native-unistyles";
import { SidebarSectionHeader } from "@/components/sidebar/sidebar-section-header";
import { density, densityIconBox, SIDEBAR_ROW_LINE_HEIGHT } from "@/styles/density";

// The placeholder draws the list it stands for (sidebar-workspace-list.tsx): the 「對話」 header, then conversation
// rows with the same height, inset and status slot, so nothing moves when the list arrives.
const ROW_OPACITIES: readonly number[] = [1, 0.85, 0.7, 0.55, 0.4];
const ROW_KEYS = ROW_OPACITIES.map((_, i) => `skeleton-row-${i}`);
const TITLE_WIDTHS = ["70%", "55%", "80%", "60%", "45%"] as const;

function SkeletonPulse({ pulse, style }: { pulse: Animated.Value; style: StyleProp<ViewStyle> }) {
  const opacity = pulse.interpolate({
    inputRange: [0, 1],
    outputRange: [0.4, 0.8],
  });

  const pulseStyle = useMemo(() => [style, { opacity }], [style, opacity]);

  return <Animated.View style={pulseStyle} />;
}

function SkeletonRow({ pulse, index }: { pulse: Animated.Value; index: number }) {
  const rowStyle = useMemo(() => [styles.row, { opacity: ROW_OPACITIES[index] }], [index]);
  const titleStyle = useMemo(() => [styles.rowTitle, { width: TITLE_WIDTHS[index] }], [index]);
  return (
    <View style={rowStyle}>
      <View style={styles.statusSlot}>
        <SkeletonPulse pulse={pulse} style={styles.rowDot} />
      </View>
      <View style={styles.titleLine}>
        <SkeletonPulse pulse={pulse} style={titleStyle} />
      </View>
    </View>
  );
}

export function SidebarAgentListSkeleton() {
  const { t } = useTranslation();
  const pulse = useRef(new Animated.Value(0)).current;

  useEffect(() => {
    const animation = Animated.loop(
      Animated.sequence([
        Animated.timing(pulse, {
          toValue: 1,
          duration: 1000,
          useNativeDriver: true,
        }),
        Animated.timing(pulse, {
          toValue: 0,
          duration: 1000,
          useNativeDriver: true,
        }),
      ]),
    );

    animation.start();
    return () => animation.stop();
  }, [pulse]);

  return (
    <View style={styles.container}>
      <SidebarSectionHeader title={t("sidebar.sections.conversations")} />
      {ROW_KEYS.map((key, index) => (
        <SkeletonRow key={key} pulse={pulse} index={index} />
      ))}
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  // The list's content padding (sidebar-workspace-list.tsx listContent).
  container: {
    flex: 1,
    paddingHorizontal: theme.spacing[2],
    paddingTop: 2,
  },
  // A conversation row (sidebar-workspace-list.tsx workspaceRow).
  row: {
    minHeight: density.rowHeight,
    marginBottom: theme.spacing[0.5],
    paddingVertical: theme.spacing[2],
    paddingLeft: theme.spacing[2],
    paddingRight: theme.spacing[3],
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
  },
  statusSlot: {
    width: densityIconBox("md"),
    height: SIDEBAR_ROW_LINE_HEIGHT,
    alignItems: "center",
    justifyContent: "center",
  },
  rowDot: {
    width: 6,
    height: 6,
    borderRadius: theme.borderRadius.full,
    backgroundColor: theme.colors.surface2,
  },
  titleLine: {
    flex: 1,
    height: SIDEBAR_ROW_LINE_HEIGHT,
    justifyContent: "center",
  },
  rowTitle: {
    height: 12,
    borderRadius: theme.borderRadius.sm,
    backgroundColor: theme.colors.surface2,
  },
}));

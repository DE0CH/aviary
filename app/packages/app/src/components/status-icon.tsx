import { useMemo, type ReactElement, type ReactNode } from "react";
import { View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import type { SidebarStateBucket } from "@/utils/sidebar-agent-state";
import type { SurfaceBackdrop } from "@/styles/surface-backdrop";
import { getStatusDotColor } from "@/utils/status-dot-color";
import { StatusRing } from "@/components/status-ring";
import { getStatusRingOffset } from "@/components/status-ring/geometry";
import { STATUS_INDICATOR_DOT_SIZE } from "@/utils/status-indicator-geometry";

interface StatusIconProps {
  children: ReactNode;
  statusBucket: SidebarStateBucket | null;
  size?: number;
  statusDotBorderColor?: string;
  backdrop: SurfaceBackdrop;
}
const DEFAULT_STATUS_DOT_OFFSET = -2;

export function StatusIcon({
  children,
  statusBucket,
  size = 14,
  statusDotBorderColor,
  backdrop,
}: StatusIconProps): ReactElement {
  const bucket = statusBucket;
  const isRunning = bucket === "running";
  // Every surfaced bucket is a dot of its own colour; needs_input is the amber one (Deyao, 2026-11-18: no glyph).
  let statusDotColor: string | undefined;
  if (bucket === "failed") statusDotColor = styles.statusDotFailed.color;
  else if (bucket === "attention") statusDotColor = styles.statusDotAttention.color;
  else if (bucket === "needs_input") statusDotColor = styles.statusDotNeedsInput.color;
  const agentIconWrapperStyle = useMemo(
    () => [styles.agentIconWrapper, { width: size, height: size }],
    [size],
  );
  const statusDotStyle = useMemo(
    () => [
      styles.statusDot,
      {
        backgroundColor: statusDotColor,
        borderColor: statusDotBorderColor ?? styles[backdrop].borderColor,
      },
    ],
    [statusDotColor, statusDotBorderColor, backdrop],
  );

  return (
    <View style={agentIconWrapperStyle}>
      {children}
      {isRunning ? (
        <View
          style={styles.statusRing}
          accessibilityRole="progressbar"
          accessibilityLabel="Agent running"
        >
          <StatusRing backdrop={backdrop} />
        </View>
      ) : null}
      {statusDotColor ? <View style={statusDotStyle} /> : null}
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  agentIconWrapper: {
    position: "relative",
    alignItems: "center",
    justifyContent: "center",
    flexShrink: 0,
  },
  statusDot: {
    position: "absolute",
    right: DEFAULT_STATUS_DOT_OFFSET,
    bottom: DEFAULT_STATUS_DOT_OFFSET,
    width: STATUS_INDICATOR_DOT_SIZE,
    height: STATUS_INDICATOR_DOT_SIZE,
    borderRadius: theme.borderRadius.full,
    borderWidth: 1,
  },
  statusRing: {
    position: "absolute",
    right: getStatusRingOffset(DEFAULT_STATUS_DOT_OFFSET, STATUS_INDICATOR_DOT_SIZE),
    bottom: getStatusRingOffset(DEFAULT_STATUS_DOT_OFFSET, STATUS_INDICATOR_DOT_SIZE),
  },
  surface0: { borderColor: theme.colors.surface0 },
  surface1: { borderColor: theme.colors.surface1 },
  surface2: { borderColor: theme.colors.surface2 },
  surfaceSidebar: { borderColor: theme.colors.surfaceSidebar },
  surfaceSidebarHover: { borderColor: theme.colors.surfaceSidebarHover },
  surfaceSidebarSelected: { borderColor: theme.colors.surfaceSidebarSelected },
  statusDotFailed: {
    color: getStatusDotColor({ theme, bucket: "failed" }) ?? undefined,
  },
  statusDotAttention: {
    color: getStatusDotColor({ theme, bucket: "attention" }) ?? undefined,
  },
  statusDotNeedsInput: {
    color: getStatusDotColor({ theme, bucket: "needs_input" }) ?? undefined,
  },
}));

import { Suspense, useEffect } from "react";
import { useWindowDimensions, View } from "react-native";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { StyleSheet } from "react-native-unistyles";
import { resolveDesktopSidebarWidth } from "@/components/desktop-sidebar-layout";
import { ChunkErrorBoundary } from "@/components/loading-frame";
import { useIsCompactFormFactor } from "@/constants/layout";
import { useIsMobilePanelActive } from "@/mobile-panels/provider";
import { requestSidebarChunkNow, loadSidebarChunkInTurn } from "@/loading/stages";
import { usePanelStore } from "@/stores/panel-store";
import { lazyWithRetry } from "@/utils/lazy-with-retry";

// The conversation list is its own chunk, fetched after the reply's display (loading/stages.ts); opening the phone
// drawer fetches it at once. Until it arrives the desktop shows the sidebar's empty column at its saved width.
const LazyLeftSidebar = lazyWithRetry(loadSidebarChunkInTurn, (chunk) => chunk.LeftSidebar);

function SidebarFallback() {
  const isCompactLayout = useIsCompactFormFactor();
  const sidebarWidth = usePanelStore((state) => state.sidebarWidth);
  const { width: windowWidth } = useWindowDimensions();
  const insets = useSafeAreaInsets();
  if (isCompactLayout) return null;
  const width = resolveDesktopSidebarWidth({
    requestedWidth: sidebarWidth,
    viewportWidth: windowWidth - insets.left - insets.right,
  });
  return <View dataSet={{ yycLoading: "" }} style={[styles.column, { width }]} />;
}

export function LeftSidebar({ active }: { active: boolean }) {
  const isCompactLayout = useIsCompactFormFactor();
  const isDrawerOpen = useIsMobilePanelActive("agent-list");
  useEffect(() => {
    if (isCompactLayout && isDrawerOpen) requestSidebarChunkNow();
  }, [isCompactLayout, isDrawerOpen]);
  return (
    <ChunkErrorBoundary name="sidebar" fallback={<SidebarFallback />}>
      <Suspense fallback={<SidebarFallback />}>
        <LazyLeftSidebar active={active} />
      </Suspense>
    </ChunkErrorBoundary>
  );
}

const styles = StyleSheet.create((theme) => ({
  column: {
    height: "100%",
    borderRightWidth: 1,
    borderRightColor: theme.colors.border,
    backgroundColor: theme.colors.surfaceSidebar,
  },
}));

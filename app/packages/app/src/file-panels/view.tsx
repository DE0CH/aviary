import { PluginClientStateProvider } from "@getpaseo/plugin/client/host";
import { createPluginClientStateSource } from "@/plugins/client-state/source";
import { Suspense, useMemo } from "react";
import type { PluginFilePanelProps } from "@getpaseo/plugin/client";
import type { PluginTheme } from "@getpaseo/plugin";
import { withUnistyles } from "react-native-unistyles";
import { useIsCompactFormFactor } from "@/constants/layout";
import { usePaneContext } from "@/panels/pane-context";
import { useHosts } from "@/runtime/host-runtime";
import { usePluginHostNavigation } from "@/plugins/host-navigation";
import { resolvePluginPlatform } from "@/plugins/platform";
import { toPluginTheme } from "@/plugins/theme";
import type { Theme } from "@/styles/theme";
import type { FilePanel } from "./panels";
import { ChunkErrorBoundary, LoadingFrame } from "@/components/loading-frame";
import { lazyWithRetry } from "@/utils/lazy-with-retry";
import { PluginFilePanelHost } from "./plugin-host";

// The built-in file views (Markdown preview with YAML front matter, source, images) are their own chunk, fetched when a
// file opens (docs/paseo-ui.md "Loading").
const BuiltinFilePanelView = lazyWithRetry(
  () => import("./builtin"),
  (m) => m.BuiltinFilePanelView,
);

type Props = Pick<PluginFilePanelProps, "file" | "tab" | "params" | "revision"> & {
  panel: FilePanel;
};
function FilePanelHost({ panel, theme, ...input }: Props & { theme: PluginTheme }) {
  const { workspaceId, serverId } = usePaneContext();
  const compact = useIsCompactFormFactor();
  const hosts = useHosts();
  const label = hosts.find((host) => host.serverId === serverId)?.label ?? serverId;
  const host = useMemo(() => ({ id: serverId, label }), [serverId, label]);
  const layout = useMemo(() => ({ compact, platform: resolvePluginPlatform() }), [compact]);
  const stateSource = useMemo(() => createPluginClientStateSource(serverId), [serverId]);
  const navigation = usePluginHostNavigation(serverId);
  const props: PluginFilePanelProps = { ...input, workspaceId, theme, host, layout, navigation };
  if (panel.builtin)
    return (
      <PluginClientStateProvider source={stateSource}>
        <ChunkErrorBoundary name="file-view">
          <Suspense fallback={<LoadingFrame />}>
            <BuiltinFilePanelView {...props} panel={panel} />
          </Suspense>
        </ChunkErrorBoundary>
      </PluginClientStateProvider>
    );
  return <PluginFilePanelHost {...props} panel={panel} />;
}
const ThemedHost = withUnistyles(FilePanelHost);
const themeMapping = (theme: Theme) => ({ theme: toPluginTheme(theme) });
export function FilePanelView(props: Props) {
  return <ThemedHost {...props} uniProps={themeMapping} />;
}

import type { ReactNode } from "react";
import { CompactHeader } from "@/components/headers/compact-header";

export { MobileWorkspaceTabSwitcher } from "./tab-switcher";

interface CompactWorkspaceHeaderProps {
  serverId: string;
  isLoading: boolean;
  title: string;
  subtitle: string;
  isSubtitleDistinct: boolean;
  onToggleExplorer: () => void;
  explorerToggleLabel: string;
  explorerToggleAccessibilityState: { expanded: boolean };
  /** Tab switcher and workspace menu, before the explorer toggle. */
  children: ReactNode;
}

/** The compact header with the workspace's project row and controls. */
export function CompactWorkspaceHeader({ isLoading, title, children }: CompactWorkspaceHeaderProps) {
  // One project, one host, no file explorer: the conversation's title and its menu (docs/paseo-ui.md)
  const actions = <>{children}</>;

  return (
    <CompactHeader
      navigation="menu"
      title={title}
      titleTestID="workspace-header-title"
      loading={isLoading}
      actions={actions}
    />
  );
}

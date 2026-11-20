import type { ReactNode } from "react";
export interface ExplorerFrameProps {
  children: ReactNode;
  presentation: "overlay" | "dock";
  persistenceKey: string;
  containerWidth: number;
  isOpen: boolean;
  isWorkspaceActive: boolean;
  surfaceColor?: string;
  workspaceColor?: string;
  /** The panel's ground: the sidebar's (the explorer), or the chat's (a conversation's side panel). */
  surface?: "sidebar" | "workspace";
}

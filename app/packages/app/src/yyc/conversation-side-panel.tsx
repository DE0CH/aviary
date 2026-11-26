/**
 * The conversation's side panel (yyc/side-panel.tsx) in upstream's right-hand panel frame: docked beside the chat on
 * a wide screen, sliding over it on a phone (the frame's mobile panel, with its swipe and backdrop to close).
 */
import React, { useEffect, useRef } from "react";
import { usePanelStore } from "@/stores/panel-store";
import { ExplorerFrame } from "@/screens/workspace/explorer-sidebar/frame";
import { ActivityPanel } from "@/yyc/activity";
import { SchedulesPanel } from "@/yyc/schedules";
import { closeSidePanel, useSidePanel } from "@/yyc/side-panel";
import { useSidePanelBack } from "@/yyc/side-panel-back";

export function ConversationSidePanel({
  sessionId,
  compact,
  persistenceKey,
  containerWidth,
  isWorkspaceActive,
}: {
  sessionId: string | null;
  compact: boolean;
  persistenceKey: string | null;
  containerWidth: number;
  isWorkspaceActive: boolean;
}) {
  const kind = useSidePanel((s) =>
    sessionId && s.shown?.sessionId === sessionId ? s.shown.kind : null
  );
  useEffect(() => {
    // a conversation left with its panel chosen: the next one starts without it
    return () => {
      if (useSidePanel.getState().shown?.sessionId === sessionId)
        useSidePanel.setState({ shown: null });
    };
  }, [sessionId]);
  // the phone's overlay and the chosen panel move together: folding with a panel beside the chat slides it over the
  // chat, and a swipe or the backdrop that puts the overlay away closes it
  const target = usePanelStore((s) => s.mobilePanel.target);
  const previous = useRef({ compact, target });
  useEffect(() => {
    const was = previous.current;
    previous.current = { compact, target };
    if (!kind || !compact) return;
    if (!was.compact) usePanelStore.getState().showMobileFileExplorer();
    else if (was.target === "file-explorer" && target !== "file-explorer")
      useSidePanel.setState({ shown: null });
  }, [compact, kind, target]);
  // on screen: beside the chat, or on a phone while the overlay is out (yyc/side-panel-state.ts)
  useSidePanelBack(kind !== null && (!compact || target === "file-explorer"), closeSidePanel);
  if (!sessionId || !persistenceKey) return null;
  return (
    <ExplorerFrame
      presentation={compact ? "overlay" : "dock"}
      surface="workspace"
      isOpen={kind !== null}
      isWorkspaceActive={isWorkspaceActive}
      persistenceKey={persistenceKey}
      containerWidth={containerWidth}
    >
      {kind === "activity" ? (
        <ActivityPanel sessionId={sessionId} compact={compact} />
      ) : null}
      {kind === "schedules" ? (
        <SchedulesPanel sessionId={sessionId} compact={compact} />
      ) : null}
    </ExplorerFrame>
  );
}

import React, { useCallback, useEffect } from "react";
import { create } from "zustand";
import { WorkspaceRenameModal } from "@/components/workspace-rename-modal";
import { useActiveWorkspaceSelection } from "@/stores/navigation-active-workspace-store";
import { useWorkspace } from "@/stores/session-store-hooks";
import { redirectIfArchivingActiveWorkspace } from "@/utils/sidebar-workspace-archive-redirect";
import { useWorkspaceArchive } from "@/workspace/use-workspace-archive";
import { ShareSheet } from "@/yyc/share";
import { sessionIdOfWorkspace } from "@/yyc/api";

type ConversationDialog = { kind: "rename" | "share" | "delete"; serverId: string; workspaceId: string };

const useConversationDialogStore = create<{
  dialog: ConversationDialog | null;
  open: (dialog: ConversationDialog) => void;
  close: () => void;
}>((set) => ({
  dialog: null,
  open: (dialog) => set({ dialog }),
  close: () => set({ dialog: null }),
}));

/** Opens a conversation dialog; menus close as soon as an item is chosen, so the dialog lives in the host below. */
export function openConversationDialog(dialog: ConversationDialog): void {
  useConversationDialogStore.getState().open(dialog);
}

/**
 * The conversation dialogs (rename, share, delete) outside any menu (docs/paseo-ui.md). Mounted once at the root.
 */
export function ConversationDialogsHost() {
  const dialog = useConversationDialogStore((state) => state.dialog);
  const close = useConversationDialogStore((state) => state.close);
  if (!dialog) return null;
  return <ConversationDialogView key={`${dialog.kind}:${dialog.workspaceId}`} dialog={dialog} onClose={close} />;
}

function ConversationDialogView({ dialog, onClose }: { dialog: ConversationDialog; onClose: () => void }) {
  const workspace = useWorkspace(dialog.serverId, dialog.workspaceId);
  const selection = useActiveWorkspaceSelection();
  const redirectAfterDelete = useCallback(() => {
    redirectIfArchivingActiveWorkspace({
      serverId: dialog.serverId,
      workspaceId: dialog.workspaceId,
      activeWorkspaceSelection: selection,
    });
  }, [dialog.serverId, dialog.workspaceId, selection]);
  const archive = useWorkspaceArchive({
    serverId: dialog.serverId,
    workspaceId: dialog.workspaceId,
    workspaceKind: "directory",
    name: workspace?.name ?? "",
    onArchiveStarted: redirectAfterDelete,
  });

  useEffect(() => {
    if (dialog.kind !== "delete") return;
    archive.archive();
    onClose();
  }, [archive, dialog.kind, onClose]);

  const sessionId = sessionIdOfWorkspace(dialog.workspaceId);
  if (dialog.kind === "rename" && workspace) {
    return (
      <WorkspaceRenameModal
        visible
        workspace={{
          serverId: dialog.serverId,
          workspaceId: dialog.workspaceId,
          name: workspace.name,
          title: workspace.title ?? null,
        }}
        onClose={onClose}
        testID="conversation-rename-modal"
      />
    );
  }
  if (dialog.kind === "share" && sessionId) {
    return <ShareSheet visible sessionId={sessionId} onClose={onClose} />;
  }
  return null;
}

import React, { useCallback, useState } from "react";
import { useTranslation } from "react-i18next";
import { Pencil, Share2, Trash2 } from "lucide-react-native";
import { withUnistyles } from "react-native-unistyles";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { WorkspaceRenameModal } from "@/components/workspace-rename-modal";
import { useActiveWorkspaceSelection } from "@/stores/navigation-active-workspace-store";
import { useWorkspace } from "@/stores/session-store-hooks";
import { useWorkspaceArchive } from "@/workspace/use-workspace-archive";
import { redirectIfArchivingActiveWorkspace } from "@/utils/sidebar-workspace-archive-redirect";
import { useShareConversation } from "@/yyc/share";
import type { Theme } from "@/styles/theme";

const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const destructiveColorMapping = (theme: Theme) => ({ color: theme.colors.destructive });
const ThemedPencil = withUnistyles(Pencil);
const ThemedShare = withUnistyles(Share2);
const ThemedTrash = withUnistyles(Trash2);

const RENAME_ICON = <ThemedPencil size={16} uniProps={mutedColorMapping} />;
const SHARE_ICON = <ThemedShare size={16} uniProps={mutedColorMapping} />;
const DELETE_ICON = <ThemedTrash size={16} uniProps={destructiveColorMapping} />;

/**
 * The conversation's own actions in the header's ⋯ menu (docs/paseo-ui.md): rename, share and delete
 * (to Recently deleted). They act on the conversation the route shows.
 */
export function ConversationMenuItems() {
  const { t } = useTranslation();
  const selection = useActiveWorkspaceSelection();
  const serverId = selection?.serverId ?? null;
  const workspaceId = selection?.workspaceId ?? null;
  const workspace = useWorkspace(serverId, workspaceId);
  const [isRenameOpen, setIsRenameOpen] = useState(false);
  const share = useShareConversation({ serverId, workspaceId });

  const redirectAfterDelete = useCallback(() => {
    if (!serverId || !workspaceId) return;
    redirectIfArchivingActiveWorkspace({
      serverId,
      workspaceId,
      activeWorkspaceSelection: selection,
    });
  }, [selection, serverId, workspaceId]);

  const archive = useWorkspaceArchive({
    serverId: serverId ?? "",
    workspaceId: workspaceId ?? "",
    workspaceKind: "directory",
    name: workspace?.name ?? "",
    onArchiveStarted: redirectAfterDelete,
  });

  const openRename = useCallback(() => setIsRenameOpen(true), []);
  const closeRename = useCallback(() => setIsRenameOpen(false), []);
  const handleDelete = useCallback(() => archive.archive(), [archive]);

  if (!serverId || !workspaceId || !workspace) return null;

  return (
    <>
      <DropdownMenuItem testID="conversation-menu-rename" leading={RENAME_ICON} onSelect={openRename}>
        {t("conversation.menu.rename")}
      </DropdownMenuItem>
      <DropdownMenuItem testID="conversation-menu-share" leading={SHARE_ICON} onSelect={share.open}>
        {t("conversation.menu.share")}
      </DropdownMenuItem>
      <DropdownMenuSeparator />
      <DropdownMenuItem
        testID="conversation-menu-delete"
        leading={DELETE_ICON}
        destructive
        onSelect={handleDelete}
      >
        {t("conversation.menu.delete")}
      </DropdownMenuItem>
      <WorkspaceRenameModal
        visible={isRenameOpen}
        workspace={{ serverId, workspaceId, name: workspace.name, title: workspace.title ?? null }}
        onClose={closeRename}
        testID="conversation-rename-modal"
      />
      {share.sheet}
    </>
  );
}

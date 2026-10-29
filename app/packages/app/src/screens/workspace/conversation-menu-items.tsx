import React, { useCallback } from "react";
import { useTranslation } from "react-i18next";
import { Pencil, Share2, Trash2 } from "lucide-react-native";
import { withUnistyles } from "react-native-unistyles";
import { DropdownMenuItem, DropdownMenuSeparator } from "@/components/ui/dropdown-menu";
import { useActiveWorkspaceSelection } from "@/stores/navigation-active-workspace-store";
import { openConversationDialog } from "@/yyc/conversation-dialog-store";
import { useSharingAvailable } from "@/yyc/me";
import type { Theme } from "@/styles/theme";

const mutedColorMapping = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const destructiveColorMapping = (theme: Theme) => ({ color: theme.colors.destructive });
const ThemedPencil = withUnistyles(Pencil);
const ThemedShare = withUnistyles(Share2);
const ThemedTrash = withUnistyles(Trash2);

// One set for both copies of the conversation menu (this header ⋯ and the sidebar row's ⋯ / right-click), sized to
// the menu item's 16 px leading slot.
export const RENAME_ICON = <ThemedPencil size={16} uniProps={mutedColorMapping} />;
export const SHARE_ICON = <ThemedShare size={16} uniProps={mutedColorMapping} />;
export const DELETE_ICON = <ThemedTrash size={16} uniProps={destructiveColorMapping} />;
/** Both copies of the menu are this wide on desktop (a phone opens a sheet). */
export const CONVERSATION_MENU_WIDTH = 220;

/**
 * The conversation's own actions in the header's ⋯ menu (docs/paseo-ui.md): rename, share and delete (to Recently
 * deleted), for the conversation the route shows. The dialogs open in ConversationDialogsHost, outside the menu.
 */
export function ConversationMenuItems() {
  const { t } = useTranslation();
  const selection = useActiveWorkspaceSelection();
  const sharing = useSharingAvailable();
  const open = useCallback(
    (kind: "rename" | "share" | "delete") => {
      if (selection) openConversationDialog({ kind, serverId: selection.serverId, workspaceId: selection.workspaceId });
    },
    [selection],
  );
  if (!selection) return null;
  return (
    <>
      <DropdownMenuItem testID="conversation-menu-rename" leading={RENAME_ICON} onSelect={() => open("rename")}>
        {t("conversation.menu.rename")}
      </DropdownMenuItem>
      {sharing ? (
        <DropdownMenuItem testID="conversation-menu-share" leading={SHARE_ICON} onSelect={() => open("share")}>
          {t("conversation.menu.share")}
        </DropdownMenuItem>
      ) : null}
      <DropdownMenuSeparator />
      <DropdownMenuItem
        testID="conversation-menu-delete"
        leading={DELETE_ICON}
        destructive
        onSelect={() => open("delete")}
      >
        {t("conversation.menu.delete")}
      </DropdownMenuItem>
    </>
  );
}

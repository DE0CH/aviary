/**
 * A conversation's ⋯ menu in the list holds pin, rename, share and delete (sidebar-workspace-menu.tsx); the row draws
 * the ⋯ when the menu has any of them. A deployment without delete (Jarvis chat, `conversationDelete: false`) still
 * has the rest: the ⋯ once came only with delete, and Jarvis chat's list had no menu at all (Deyao, 2026-11-20).
 */
export function conversationMenuHasItems(actions: {
  onTogglePin?: () => void;
  onRename?: () => void;
  canShare: boolean;
  onArchive?: () => void;
}): boolean {
  return Boolean(actions.onTogglePin || actions.onRename || actions.canShare || actions.onArchive);
}

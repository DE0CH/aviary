import { create } from "zustand";

// What the conversation menus open (rename, share, delete); the dialogs themselves are yyc/conversation-dialogs.tsx,
// in the hosts chunk, so the menus carry only this store (docs/paseo-ui.md "Loading").
export type ConversationDialog = {
  kind: "rename" | "share" | "delete";
  serverId: string;
  workspaceId: string;
};

export const useConversationDialogStore = create<{
  dialog: ConversationDialog | null;
  open: (dialog: ConversationDialog) => void;
  close: () => void;
}>((set) => ({
  dialog: null,
  open: (dialog) => set({ dialog }),
  close: () => set({ dialog: null }),
}));

/** Opens a conversation dialog; menus close as soon as an item is chosen, so the dialog lives in its host. */
export function openConversationDialog(dialog: ConversationDialog): void {
  useConversationDialogStore.getState().open(dialog);
}

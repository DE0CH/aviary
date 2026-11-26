import { useEffect } from "react";
import { systemBackPress } from "@/components/ui/isolated-bottom-sheet-modal/back-press";
import type { BackPressSource } from "@/components/ui/isolated-bottom-sheet-modal/visibility-tracker";

/**
 * Android's Back closes a conversation's side panel while it is on screen (docked beside the chat, or the phone's
 * overlay), as it closes a sheet; with none on screen it is the system's. Without it Back on an open Activity panel
 * left the app for the launcher (the Fold's emulator walk, 2026-11-26).
 */
export function useSidePanelBack(
  open: boolean,
  close: () => void,
  backPress: BackPressSource = systemBackPress,
): void {
  useEffect(() => {
    if (!open) return;
    return backPress.subscribe(() => {
      close();
      return true;
    });
  }, [open, close, backPress]);
}

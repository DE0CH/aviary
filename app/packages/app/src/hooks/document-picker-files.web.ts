import type { SelectedFile } from "@/attachments/selected-file";

// The web build picks files with a file input (hooks/use-file-picker.ts), so expo-document-picker and
// expo-file-system stay out of the web entry chunk (docs/paseo-ui.md "Loading")
export function pickFilesWithDocumentPicker(): Promise<SelectedFile[] | null> {
  return Promise.reject(new Error("The document picker is native only."));
}

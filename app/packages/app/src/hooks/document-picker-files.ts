import * as DocumentPicker from "expo-document-picker";
import { File } from "expo-file-system";
import { getMimeTypeFromPath } from "@/attachments/file-types";
import type { SelectedFile } from "@/attachments/selected-file";

/** Native: the system document picker. The web build uses a file input instead (document-picker-files.web.ts). */
export async function pickFilesWithDocumentPicker(): Promise<SelectedFile[] | null> {
  const result = await DocumentPicker.getDocumentAsync({
    multiple: true,
    copyToCacheDirectory: true,
  });

  if (result.canceled || result.assets.length === 0) {
    return null;
  }

  return result.assets.map((asset) => ({
    fileName: asset.name,
    mimeType: asset.mimeType ?? getMimeTypeFromPath(asset.name),
    readBytes: () => new File(asset.uri).bytes(),
  }));
}

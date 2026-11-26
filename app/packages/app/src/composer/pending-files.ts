import type { SelectedFile } from "@/attachments/selected-file";
import type { UploadProgress } from "@/yyc/upload-progress";

/**
 * The message box's chips for files still uploading (composer/index.tsx `uploadSelectedFiles`): one per file from the
 * pick or drop until its upload ends, drawing its progress. While any is listed the send waits.
 */
export interface PendingFileAttachment {
  id: number;
  file: SelectedFile;
  /** How far its upload has come (yyc/asset-upload.ts). */
  progress?: UploadProgress;
}

/** The list with one chip's progress replaced. */
export function withPendingFileProgress(
  pending: readonly PendingFileAttachment[],
  id: number,
  progress: UploadProgress,
): PendingFileAttachment[] {
  return pending.map((entry) => (entry.id === id ? { ...entry, progress } : entry));
}

/**
 * The list without the chips of an upload that ended, by id: a progress report replaced each chip with a new object,
 * so the placeholders made at the start are no longer the list's entries.
 */
export function withoutPendingFiles(
  pending: readonly PendingFileAttachment[],
  ended: readonly PendingFileAttachment[],
): PendingFileAttachment[] {
  const endedIds = new Set(ended.map((entry) => entry.id));
  return pending.filter((entry) => !endedIds.has(entry.id));
}

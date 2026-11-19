/**
 * What the first load needs of uploads (the message box's chips and the send): the progress shape, its share, and
 * how long the server keeps an unsent upload. The uploader itself (yyc/asset-upload.ts) is fetched when something is
 * attached, so it stays out of the entry chunk (docs/paseo-ui.md "Budgets").
 */

/** How far an upload has come: bytes sent of the whole request body. */
export interface UploadProgress {
  sent: number;
  total: number;
}

/** The share of an upload done, 0 to 1 (0 before the first report). */
export function uploadFraction(progress: UploadProgress | undefined): number {
  if (!progress || progress.total <= 0) return 0;
  return Math.min(1, Math.max(0, progress.sent / progress.total));
}

/** The server keeps an unsent upload this long (magpie `UNATTACHED_UPLOAD`, 24 h); a send names it only within this. */
export const UPLOAD_KEPT_MS = 23 * 60 * 60 * 1000;

/** Whether a picture uploaded at `uploadedAtMs` can still be named by its id at `nowMs`. */
export function uploadStillKept(uploadedAtMs: number | undefined, nowMs: number): boolean {
  return uploadedAtMs !== undefined && nowMs - uploadedAtMs < UPLOAD_KEPT_MS;
}

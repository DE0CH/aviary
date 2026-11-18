import { apiUrl } from "@/yyc/api";
import { accessHeaders } from "@/yyc/access-token";

/**
 * An upload to the server's `POST /api/assets/files` (pigeon `uploads.rs`), made when a file or a picture is attached:
 * the message box shows its progress, and the send only names what was uploaded (a file by its `path`, a picture by
 * its `uploadId`). A picture goes up as an image (`?kind=image`: the server keeps the copy the model sees), so a
 * message sent during a reply and one sent between replies carry the same references.
 *
 * XMLHttpRequest, not fetch: only it reports the bytes sent (web and React Native alike).
 */

export type AssetKind = "file" | "image";

export interface UploadedAsset {
  path: string;
  name: string;
  size: number;
  mimeType: string;
  /** The server's id of the upload; what a send names an uploaded picture by. */
  uploadId?: string;
}

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

/** The part of XMLHttpRequest an upload uses (a test gives its own). */
export interface UploadXhr {
  open(method: string, url: string): void;
  setRequestHeader(name: string, value: string): void;
  send(body: FormData): void;
  abort(): void;
  withCredentials: boolean;
  status: number;
  responseText: string;
  upload: { onprogress: ((event: { loaded: number; total: number }) => void) | null };
  onload: (() => void) | null;
  onerror: (() => void) | null;
  onabort: (() => void) | null;
}

export interface UploadAssetInput {
  fileName: string;
  mimeType: string;
  bytes: Uint8Array;
  kind: AssetKind;
  onProgress?: (progress: UploadProgress) => void;
  signal?: AbortSignal;
  /** For tests. */
  createXhr?: () => UploadXhr;
}

/** Uploads one file or picture; rejects with the server's sentence when it refuses. */
export function uploadAsset(input: UploadAssetInput): Promise<UploadedAsset> {
  return new Promise((resolve, reject) => {
    const xhr = input.createXhr ? input.createXhr() : (new XMLHttpRequest() as unknown as UploadXhr);
    const form = new FormData();
    form.append(
      "files",
      new Blob([input.bytes as Uint8Array<ArrayBuffer>], { type: input.mimeType }),
      input.fileName,
    );
    xhr.open("POST", apiUrl(`/api/assets/files${input.kind === "image" ? "?kind=image" : ""}`));
    xhr.withCredentials = true;
    for (const [name, value] of Object.entries(accessHeaders())) xhr.setRequestHeader(name, value);
    xhr.upload.onprogress = (event) => {
      input.onProgress?.({ sent: event.loaded, total: event.total });
    };
    xhr.onload = () => {
      let body: {
        attachments?: Array<{ path: string; name: string; size: number; mimeType: string; uploadId?: string }>;
        error?: string;
      } | null = null;
      try {
        body = JSON.parse(xhr.responseText);
      } catch {
        body = null;
      }
      const stored = body?.attachments?.[0];
      if (xhr.status < 200 || xhr.status >= 300 || !stored) {
        reject(new Error(body?.error ?? "Upload failed."));
        return;
      }
      resolve({
        path: stored.path,
        name: stored.name,
        size: stored.size,
        mimeType: stored.mimeType,
        ...(stored.uploadId ? { uploadId: stored.uploadId } : {}),
      });
    };
    xhr.onerror = () => reject(new Error("Upload failed."));
    xhr.onabort = () => reject(new DOMException("Upload cancelled.", "AbortError"));
    input.signal?.addEventListener("abort", () => xhr.abort(), { once: true });
    xhr.send(form);
  });
}

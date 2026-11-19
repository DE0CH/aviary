import { apiUrl } from "@/yyc/api";
import { accessHeaders } from "@/yyc/access-token";
import { getAttachmentStore } from "@/attachments/store";
import type { AttachmentMetadata } from "@/attachments/types";
import { base64ToBytes } from "@/utils/bytes";
import type { UploadProgress } from "@/yyc/upload-progress";

/**
 * An upload to the server's `POST /api/assets/files` (pigeon `uploads.rs`), made when a file or a picture is attached:
 * the message box shows its progress, and the send only names what was uploaded (a file by its `path`, a picture by
 * its `uploadId`). A picture goes up as an image (`?kind=image`: the server keeps the copy the model sees), so a
 * message sent during a reply and one sent between replies carry the same references.
 *
 * XMLHttpRequest, not fetch: only it reports the bytes sent (web and React Native alike). Fetched with import() when
 * something is attached (composer/index.tsx, composer/actions.ts): a chunk of its own, out of the entry.
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

/** The bytes of a picture kept in the app's attachment store. */
async function attachmentBytes(attachment: AttachmentMetadata): Promise<Uint8Array> {
  const store = await getAttachmentStore();
  return base64ToBytes(await store.encodeBase64({ attachment }));
}

/**
 * Uploads a picture just attached to the message box (yyc/asset-upload.ts); what its metadata keeps so the send
 * names the upload instead of carrying the bytes. Null when the server names no upload: a pigeon before
 * `?kind=image` (YYC AI and the Jarvis chat move their pins apart) took it as a file, and the send carries the bytes.
 */
export async function uploadAttachedImage(
  image: AttachmentMetadata,
  onProgress: (progress: UploadProgress) => void,
): Promise<Pick<AttachmentMetadata, "uploadId" | "uploadedAtMs"> | null> {
  const bytes = await attachmentBytes(image);
  const extension = image.mimeType.split("/")[1]?.replace("jpeg", "jpg") ?? "png";
  const uploaded = await uploadAsset({
    fileName: image.fileName ?? `image.${extension}`,
    mimeType: image.mimeType,
    bytes,
    kind: "image",
    onProgress,
  });
  return uploaded.uploadId ? { uploadId: uploaded.uploadId, uploadedAtMs: Date.now() } : null;
}

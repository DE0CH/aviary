import { getAttachmentStore } from "@/attachments/store";
import type { AttachmentMetadata } from "@/attachments/types";
import { base64ToBytes } from "@/utils/bytes";
import { uploadAsset, type UploadProgress } from "@/yyc/asset-upload";

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

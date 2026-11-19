import type { AttachmentMetadata } from "@/attachments/types";
import { encodeAttachmentsForSend, type SendImage } from "@/attachments/service";

type ImageInput = AttachmentMetadata;

export async function encodeImages(images?: ImageInput[]): Promise<SendImage[] | undefined> {
  return await encodeAttachmentsForSend(images);
}

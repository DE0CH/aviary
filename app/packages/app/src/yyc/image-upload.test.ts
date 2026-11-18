import { afterEach, describe, expect, it } from "vitest";
import { __setAttachmentStoreForTests } from "@/attachments/store";
import type { AttachmentMetadata, AttachmentStore } from "@/attachments/types";
import { uploadAttachedImage } from "./asset-upload";

const picture: AttachmentMetadata = {
  id: "att_1",
  mimeType: "image/png",
  storageType: "web-indexeddb",
  storageKey: "att_1",
  fileName: "photo.png",
  createdAt: 1,
};

/** An XMLHttpRequest that answers every upload with `answer`, recording the URLs. */
function serverAnswering(answer: Record<string, unknown>, urls: string[]) {
  return class {
    url = "";
    withCredentials = false;
    status = 0;
    responseText = "";
    upload: { onprogress: ((e: { loaded: number; total: number }) => void) | null } = { onprogress: null };
    onload: (() => void) | null = null;
    onerror: (() => void) | null = null;
    onabort: (() => void) | null = null;
    open(_m: string, url: string) {
      this.url = url;
    }
    setRequestHeader() {}
    abort() {}
    send() {
      urls.push(this.url);
      this.status = 201;
      this.responseText = JSON.stringify({ attachments: [answer] });
      this.onload?.();
    }
  };
}

const OriginalXhr = globalThis.XMLHttpRequest;
afterEach(() => {
  globalThis.XMLHttpRequest = OriginalXhr;
});

describe("uploadAttachedImage", () => {
  __setAttachmentStoreForTests({
    encodeBase64: async () => "aGVsbG8=",
  } as unknown as AttachmentStore);
  const stored = { path: "uploads/u1/photo.png", name: "photo.png", size: 5, mimeType: "image/png" };

  it("uploads the picture as an image and keeps the upload's id", async () => {
    const urls: string[] = [];
    globalThis.XMLHttpRequest = serverAnswering({ ...stored, uploadId: "u1" }, urls) as never;
    const got = await uploadAttachedImage(picture, () => {});
    expect(urls[0]).toMatch(/\/api\/assets\/files\?kind=image$/);
    expect(got?.uploadId).toBe("u1");
    expect(typeof got?.uploadedAtMs).toBe("number");
  });

  it("is null when the server names no upload (a pigeon before ?kind=image): the send carries the bytes", async () => {
    globalThis.XMLHttpRequest = serverAnswering(stored, []) as never;
    await expect(uploadAttachedImage(picture, () => {})).resolves.toBeNull();
  });
});

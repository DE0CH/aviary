import { afterEach, describe, expect, it, vi } from "vitest";
import { uploadAsset, type UploadXhr } from "./asset-upload";
import { UPLOAD_KEPT_MS, uploadFraction, uploadStillKept } from "./upload-progress";

class FakeXhr implements UploadXhr {
  method = "";
  url = "";
  headers: Record<string, string> = {};
  body: Uint8Array | null = null;
  aborted = false;
  withCredentials = false;
  status = 0;
  responseText = "";
  upload: UploadXhr["upload"] = { onprogress: null };
  onload: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onabort: (() => void) | null = null;
  open(method: string, url: string) {
    this.method = method;
    this.url = url;
  }
  setRequestHeader(name: string, value: string) {
    this.headers[name] = value;
  }
  send(body: Uint8Array) {
    this.body = body;
  }
  abort() {
    this.aborted = true;
    this.onabort?.();
  }
  answer(status: number, body: unknown) {
    this.status = status;
    this.responseText = JSON.stringify(body);
    this.onload?.();
  }
}

/** The `files` part of what the fake was sent, read back by the platform's own multipart parser. */
async function sentFile(xhr: FakeXhr): Promise<File> {
  const response = new Response(xhr.body as Uint8Array<ArrayBuffer>, {
    headers: { "content-type": xhr.headers["Content-Type"] ?? "" },
  });
  // the platform's FormData; the app's types are React Native's, which has no get()
  const form = (await response.formData()) as unknown as { get(name: string): unknown };
  return form.get("files") as File;
}

function start(kind: "file" | "image", fileName = "photo.png") {
  const xhr = new FakeXhr();
  const progress: Array<{ sent: number; total: number }> = [];
  const result = uploadAsset({
    fileName,
    mimeType: "image/png",
    bytes: new Uint8Array([1, 2, 3]),
    kind,
    onProgress: (p) => progress.push(p),
    createXhr: () => xhr,
  });
  return { xhr, progress, result };
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("uploadAsset", () => {
  it("uploads a picture as an image, reports its progress and gives the upload's id", async () => {
    const { xhr, progress, result } = start("image");
    expect(xhr.method).toBe("POST");
    expect(xhr.url).toMatch(/\/api\/assets\/files\?kind=image$/);
    expect(xhr.withCredentials).toBe(true);
    const file = await sentFile(xhr);
    expect(file.name).toBe("photo.png");
    expect(file.type).toBe("image/png");
    expect(new Uint8Array(await file.arrayBuffer())).toEqual(new Uint8Array([1, 2, 3]));
    xhr.upload.onprogress?.({ loaded: 1, total: 4 });
    xhr.upload.onprogress?.({ loaded: 4, total: 4 });
    xhr.answer(201, {
      attachments: [
        { path: "uploads/u1/photo.png", name: "photo.png", size: 3, mimeType: "image/png", uploadId: "u1" },
      ],
    });
    await expect(result).resolves.toEqual({
      path: "uploads/u1/photo.png",
      name: "photo.png",
      size: 3,
      mimeType: "image/png",
      uploadId: "u1",
    });
    expect(progress).toEqual([
      { sent: 1, total: 4 },
      { sent: 4, total: 4 },
    ]);
  });

  it("sends a name with a quote and Chinese characters as the server will read it", async () => {
    const { xhr } = start("file", '報告 "終稿".pdf');
    expect((await sentFile(xhr)).name).toBe('報告 "終稿".pdf');
  });

  it("builds its body without a Blob of bytes, which React Native cannot make", async () => {
    vi.stubGlobal(
      "Blob",
      class {
        constructor(parts: unknown[] = []) {
          if (parts.some((part) => part instanceof ArrayBuffer || ArrayBuffer.isView(part))) {
            throw new Error("Creating blobs from 'ArrayBuffer' and 'ArrayBufferView' are not supported");
          }
        }
      },
    );
    const { xhr, result } = start("file");
    xhr.answer(201, {
      attachments: [{ path: "uploads/u2/photo.png", name: "photo.png", size: 3, mimeType: "image/png" }],
    });
    await expect(result).resolves.toMatchObject({ path: "uploads/u2/photo.png" });
  });

  it("uploads a file without the image kind", () => {
    const { xhr } = start("file");
    expect(xhr.url).toMatch(/\/api\/assets\/files$/);
  });

  it("rejects with the server's sentence", async () => {
    const { xhr, result } = start("image");
    xhr.answer(415, { error: "This kind of file is not supported." });
    await expect(result).rejects.toThrow("This kind of file is not supported.");
  });

  it("rejects when the connection fails", async () => {
    const { xhr, result } = start("file");
    xhr.onerror?.();
    await expect(result).rejects.toThrow("Upload failed.");
  });
});

describe("uploadFraction", () => {
  it("is the share sent, 0 before any report", () => {
    expect(uploadFraction(undefined)).toBe(0);
    expect(uploadFraction({ sent: 0, total: 0 })).toBe(0);
    expect(uploadFraction({ sent: 1, total: 4 })).toBe(0.25);
    expect(uploadFraction({ sent: 5, total: 4 })).toBe(1);
  });
});

describe("uploadStillKept", () => {
  it("names an upload by its id only while the server keeps it", () => {
    expect(uploadStillKept(undefined, 0)).toBe(false);
    expect(uploadStillKept(0, UPLOAD_KEPT_MS - 1)).toBe(true);
    expect(uploadStillKept(0, UPLOAD_KEPT_MS)).toBe(false);
  });
});

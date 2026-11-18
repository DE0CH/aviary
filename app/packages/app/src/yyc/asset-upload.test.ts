import { describe, expect, it } from "vitest";
import { uploadAsset, type UploadXhr } from "./asset-upload";
import { UPLOAD_KEPT_MS, uploadFraction, uploadStillKept } from "./upload-progress";

class FakeXhr implements UploadXhr {
  method = "";
  url = "";
  headers: Record<string, string> = {};
  body: FormData | null = null;
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
  send(body: FormData) {
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

function start(kind: "file" | "image") {
  const xhr = new FakeXhr();
  const progress: Array<{ sent: number; total: number }> = [];
  const result = uploadAsset({
    fileName: "photo.png",
    mimeType: "image/png",
    bytes: new Uint8Array([1, 2, 3]),
    kind,
    onProgress: (p) => progress.push(p),
    createXhr: () => xhr,
  });
  return { xhr, progress, result };
}

describe("uploadAsset", () => {
  it("uploads a picture as an image, reports its progress and gives the upload's id", async () => {
    const { xhr, progress, result } = start("image");
    expect(xhr.method).toBe("POST");
    expect(xhr.url).toMatch(/\/api\/assets\/files\?kind=image$/);
    expect(xhr.withCredentials).toBe(true);
    expect((xhr.body?.get("files") as File).name).toBe("photo.png");
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

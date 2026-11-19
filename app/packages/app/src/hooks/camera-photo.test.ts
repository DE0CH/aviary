import { describe, expect, it, vi } from "vitest";
import { takeCameraPhoto, type CameraPicker } from "./camera-photo";

function picker(overrides: Partial<CameraPicker> = {}): CameraPicker {
  return {
    requestCameraPermissionsAsync: vi.fn(async () => ({ granted: true })),
    launchCameraAsync: vi.fn(async () => ({
      canceled: false,
      assets: [{ uri: "file:///tmp/photo.jpg", mimeType: "image/jpeg", fileName: "photo.jpg" }],
    })),
    ...overrides,
  };
}

describe("takeCameraPhoto", () => {
  it("returns the photo the camera took", async () => {
    const p = picker();
    const result = await takeCameraPhoto(p);
    expect(result).toEqual({
      kind: "photo",
      assets: [{ uri: "file:///tmp/photo.jpg", mimeType: "image/jpeg", fileName: "photo.jpg" }],
    });
    expect(p.launchCameraAsync).toHaveBeenCalledWith({ mediaTypes: ["images"], quality: 0.8 });
  });

  it("never opens the camera when the person refused it", async () => {
    const p = picker({ requestCameraPermissionsAsync: vi.fn(async () => ({ granted: false })) });
    expect(await takeCameraPhoto(p)).toEqual({ kind: "denied" });
    expect(p.launchCameraAsync).not.toHaveBeenCalled();
  });

  it("is cancelled when the camera is closed without a photo", async () => {
    const closed = picker({ launchCameraAsync: vi.fn(async () => ({ canceled: true, assets: null })) });
    expect(await takeCameraPhoto(closed)).toEqual({ kind: "cancelled" });
    const empty = picker({ launchCameraAsync: vi.fn(async () => ({ canceled: false, assets: [] })) });
    expect(await takeCameraPhoto(empty)).toEqual({ kind: "cancelled" });
  });
});

import type { ExpoImagePickerAssetLike } from "./picked-image-normalizer";

/** The part of expo-image-picker a camera photo needs (injected, so the flow is tested without the native module). */
export interface CameraPicker {
  requestCameraPermissionsAsync: () => Promise<{ granted: boolean }>;
  launchCameraAsync: (options: {
    mediaTypes: "images"[];
    quality: number;
  }) => Promise<{ canceled: boolean; assets?: readonly ExpoImagePickerAssetLike[] | null }>;
}

export type CameraPhotoResult =
  | { kind: "photo"; assets: readonly ExpoImagePickerAssetLike[] }
  | { kind: "cancelled" }
  | { kind: "denied" };

/**
 * One photo from the system camera for the message box (docs/paseo-ui.md "The iOS app"): asks for the camera when
 * it has not been allowed yet, and never opens it when the person said no.
 */
export async function takeCameraPhoto(picker: CameraPicker): Promise<CameraPhotoResult> {
  const permission = await picker.requestCameraPermissionsAsync();
  if (!permission.granted) return { kind: "denied" };
  const result = await picker.launchCameraAsync({ mediaTypes: ["images"], quality: 0.8 });
  if (result.canceled || !result.assets?.length) return { kind: "cancelled" };
  return { kind: "photo", assets: result.assets };
}

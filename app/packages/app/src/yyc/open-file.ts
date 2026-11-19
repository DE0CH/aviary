import { Linking } from "react-native";
import * as FileSystem from "expo-file-system/legacy";
import * as Sharing from "expo-sharing";
import { isWeb } from "@/constants/platform";
import { accessHeaders } from "@/yyc/access-token";
import { absoluteUrl } from "@/yyc/api";
import { siteOrigin } from "@/yyc/site";

/**
 * Opens a file the server hands out (a sent file, a path a reply names, a shared conversation's file). The browser
 * opens the URL with Access's cookie. The iOS app downloads it with its Access token first (Safari would not have
 * one) and offers it in the share sheet, which previews it and saves it to Files.
 */
export async function openServerFile(url: string, fileName: string): Promise<void> {
  if (isWeb) {
    await Linking.openURL(url);
    return;
  }
  const safeName = fileName.replace(/[/\\]/g, "_") || "file";
  const target = `${FileSystem.cacheDirectory}yyc-${Date.now()}-${safeName}`;
  const result = await FileSystem.downloadAsync(url, target, { headers: accessHeaders() });
  if (result.status < 200 || result.status >= 300) throw new Error(`HTTP ${result.status}`);
  await Sharing.shareAsync(result.uri);
}

export interface ImageSource {
  uri: string;
  headers?: Record<string, string>;
}

/**
 * The source of any image the app shows: a path on the server (the history's images, `/api/blobs/…`) is read from
 * the server, and the iOS app sends its Access token to the server (a page has Access's cookie). Anything else (a
 * local file, `blob:`, `data:`, another site) is loaded as it is.
 */
export function serverImageSource(url: string): ImageSource {
  const uri = url.startsWith("/") && !url.startsWith("//") ? absoluteUrl(url) : url;
  const origin = siteOrigin();
  if (isWeb || !origin || !uri.startsWith(`${origin}/`)) return { uri };
  return { uri, headers: accessHeaders() };
}

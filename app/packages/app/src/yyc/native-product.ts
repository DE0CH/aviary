/**
 * A native build's product name (app.config.js `extra.productName`), given to yyc/product.ts when the app starts.
 * Its own module: product.ts is bundled with the translations for the page's first paint and must not pull in Expo.
 */
import Constants from "expo-constants";
import { isNative } from "@/constants/platform";
import { initialProductName, setProductName } from "@/yyc/product";

export function applyBuildProductName(): void {
  if (!isNative) return;
  const name = (Constants.expoConfig?.extra as { productName?: unknown } | undefined)?.productName;
  setProductName(initialProductName(null, name));
}

/** The logo a native build carries (app.config.js `extra.brand`): its launch image. Not used on the web. */
import Constants from "expo-constants";

const LOGOS: Record<string, number> = {
  yyc: require("../../assets/images/yyc-splash.png"),
  "jarvis-chat": require("../../assets/images/jarvis-chat-splash.png"),
};

export function bundledBrandLogo(): number | null {
  const brand = (Constants.expoConfig?.extra as { brand?: unknown } | undefined)?.brand;
  return typeof brand === "string" ? (LOGOS[brand] ?? null) : null;
}

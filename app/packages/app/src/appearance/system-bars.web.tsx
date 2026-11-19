import { useEffect } from "react";
import { useUnistyles } from "react-native-unistyles";
import { setThemeColor } from "./theme-color";

/** Web: the browser's bar and an installed app's status bar take the theme's background (theme-color.ts). */
export function AppearanceSystemBars() {
  // The hook, not a stylesheet: on web a stylesheet's theme values are CSS variables, the hook's are the colours
  const { theme } = useUnistyles();
  const color = theme.colors.surface0;
  useEffect(() => {
    if (typeof document !== "undefined") setThemeColor(document, color);
  }, [color]);
  return null;
}

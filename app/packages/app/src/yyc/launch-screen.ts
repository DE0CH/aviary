/**
 * iOS's launch screen stays over the app until something hides it. expo-router hides it once its navigator is ready,
 * but the native app's sign-in screen (access-gate.tsx) is drawn before the router mounts, so nothing did: Jarvis Chat
 * showed its launch image over the sign-in screen for ever (docs/paseo-ui.md "The iOS app"). The gate hides it itself
 * when it shows its own screen.
 */
import { useEffect } from "react";
import * as SplashScreen from "expo-splash-screen";
import { isNative } from "@/constants/platform";

export function useHideLaunchScreen(when: boolean): void {
  useEffect(() => {
    if (!when || !isNative) return;
    try {
      SplashScreen.hide();
    } catch {
      // a build without the native module has no launch screen to hide
    }
  }, [when]);
}

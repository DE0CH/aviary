// Install runtime polyfills before any other module evaluates. Keep this the first import.
// oxlint-disable-next-line import/no-unassigned-import -- Polyfills install on import.
import "./src/polyfills/install-early";

// Configure Unistyles before Expo Router pulls in any components using StyleSheet.
import "./src/styles/unistyles";
// oxlint-disable-next-line import/no-unassigned-import -- Preserve Expo's entry side effects.
import "@expo/metro-runtime";
// oxlint-disable-next-line import/no-unassigned-import -- Preserve Expo's entry side effects.
import "expo-router/build/fast-refresh";
import { renderRootComponent } from "expo-router/build/renderRootComponent";
import { startWatchdog } from "./src/watchdog";
import { RootApp } from "./src/root-app";
import { isWeb } from "./src/constants/platform";
import { i18nReady } from "./src/i18n/i18next";
import { startLoadingStages } from "./src/loading/stages";
import { startHostRuntimeBootstrapOnce } from "./src/runtime/host-runtime-start";
import { isSignedOutPage } from "./src/yyc/signed-out-shell";

startWatchdog();
if (isWeb) {
  // On web the connection and the loading stages start as the entry runs (loading/stages.ts, docs/paseo-ui.md
  // "Loading"), never on a page the server marked signed out (AccessGate renders no host runtime there); native starts
  // the connection from app/_layout.tsx. The page's language arrives as its own chunk meanwhile, and the app renders
  // once it has (the page frame and the early message box are there and typable until then).
  if (!isSignedOutPage()) {
    startHostRuntimeBootstrapOnce();
    startLoadingStages();
  }
  void i18nReady.then(() => renderRootComponent(RootApp));
} else {
  renderRootComponent(RootApp);
}

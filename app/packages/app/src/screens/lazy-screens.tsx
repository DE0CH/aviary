import { Suspense, type ComponentType, type LazyExoticComponent, type ReactElement } from "react";
import { ChunkErrorBoundary, LoadingFrame } from "@/components/loading-frame";
import { lazyWithRetry } from "@/utils/lazy-with-retry";
import type { SettingsScreenProps } from "./settings-screen";

// Screens nobody lands on first are each their own chunk, fetched when opened (docs/paseo-ui.md "Loading").
function lazyScreen<Props extends object>(
  name: string,
  Screen: LazyExoticComponent<ComponentType<Props>>,
): (props: Props) => ReactElement {
  function LazyScreen(props: Props) {
    return (
      <ChunkErrorBoundary name={name}>
        <Suspense fallback={<LoadingFrame />}>
          <Screen {...props} />
        </Suspense>
      </ChunkErrorBoundary>
    );
  }
  LazyScreen.displayName = `LazyScreen(${name})`;
  return LazyScreen;
}

export const SettingsScreen = lazyScreen<SettingsScreenProps>(
  "settings",
  lazyWithRetry(
    () => import("./settings-screen"),
    (m) => m.default,
  ),
);

export const SchedulesScreen = lazyScreen<object>(
  "schedules",
  lazyWithRetry(
    () => import("./schedules-screen"),
    (m) => m.SchedulesScreen,
  ),
);

export const SessionsScreen = lazyScreen<object>(
  "sessions",
  lazyWithRetry(
    () => import("./sessions-screen"),
    (m) => m.SessionsScreen,
  ),
);

export const OpenProjectScreen = lazyScreen<object>(
  "open-project",
  lazyWithRetry(
    () => import("./open-project-screen"),
    (m) => m.OpenProjectScreen,
  ),
);

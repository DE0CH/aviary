import { Suspense, useState, type ComponentType, type ReactElement } from "react";
import { ChunkErrorBoundary, LoadingFrame } from "@/components/loading-frame";
import { lazyWithRetry } from "@/utils/lazy-with-retry";

/**
 * A component that is its own chunk, fetched the first time it renders (docs/paseo-ui.md "Loading"): retried by
 * lazyWithRetry, an empty LoadingFrame until it arrives (or `fallback`), a failure kept local.
 */
export function lazyArea<Module, Props extends object>(
  name: string,
  load: () => Promise<Module>,
  pick: (module: Module) => ComponentType<Props>,
  fallback: ReactElement | null = <LoadingFrame />
): (props: Props) => ReactElement {
  const Lazy = lazyWithRetry(load, pick);
  function LazyArea(props: Props) {
    return (
      <ChunkErrorBoundary name={name}>
        <Suspense fallback={fallback}>
          <Lazy {...props} />
        </Suspense>
      </ChunkErrorBoundary>
    );
  }
  LazyArea.displayName = `LazyArea(${name})`;
  return LazyArea;
}

/**
 * A sheet or dialog that is its own chunk: nothing is fetched or drawn until `visible` is first true; from then on it
 * stays mounted, so it can animate closed.
 */
export function lazySheet<Module, Props extends { visible: boolean }>(
  name: string,
  load: () => Promise<Module>,
  pick: (module: Module) => ComponentType<Props>
): (props: Props) => ReactElement | null {
  const Area = lazyArea(name, load, pick, null);
  function LazySheet(props: Props) {
    const [opened, setOpened] = useState(props.visible);
    if (props.visible && !opened) setOpened(true);
    return opened || props.visible ? <Area {...props} /> : null;
  }
  LazySheet.displayName = `LazySheet(${name})`;
  return LazySheet;
}

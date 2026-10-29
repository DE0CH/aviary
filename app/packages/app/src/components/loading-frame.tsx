import { Component, type ReactNode } from "react";
import { View, type StyleProp, type ViewStyle } from "react-native";
import { isChunkLoadError } from "@/utils/lazy-with-retry";

/**
 * What a lazy area shows while its chunk is on the way: an empty area of the app's own background. It carries
 * data-yyc-loading, which tells public/index.html's early message box that the app is still arriving
 * (docs/paseo-ui.md "Loading").
 */
export function LoadingFrame({ style }: { style?: StyleProp<ViewStyle> }) {
  return <View dataSet={{ yycLoading: "" }} style={[{ flex: 1 }, style]} />;
}

/**
 * Keeps a lazy area's chunk that could not be fetched from taking the rest of the app down with it. Any other error
 * (a render bug below it) goes on up to RootErrorBoundary, which shows it with a Reload button.
 */
export class ChunkErrorBoundary extends Component<
  { name: string; fallback?: ReactNode; children: ReactNode },
  { error: unknown }
> {
  state: { error: unknown } = { error: null };

  static getDerivedStateFromError(error: unknown): { error: unknown } {
    return { error: error ?? new Error("unknown render error") };
  }

  componentDidCatch(error: unknown): void {
    if (isChunkLoadError(error)) {
      console.error(`[Loading] The ${this.props.name} chunk failed to load`, error);
    }
  }

  render(): ReactNode {
    const { error } = this.state;
    if (error === null) return this.props.children;
    if (!isChunkLoadError(error)) throw error;
    return this.props.fallback ?? null;
  }
}

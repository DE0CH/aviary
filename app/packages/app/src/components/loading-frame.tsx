import { Component, type ReactNode } from "react";
import { View, type StyleProp, type ViewStyle } from "react-native";

/**
 * What a lazy area shows while its chunk is on the way: an empty area of the app's own background. It carries
 * data-yyc-loading, which tells public/index.html's early message box that the app is still arriving
 * (docs/paseo-ui.md "Loading").
 */
export function LoadingFrame({ style }: { style?: StyleProp<ViewStyle> }) {
  return <View dataSet={{ yycLoading: "" }} style={[{ flex: 1 }, style]} />;
}

/** Keeps a lazy area's failed chunk from taking the rest of the app down with it. */
export class ChunkErrorBoundary extends Component<
  { name: string; fallback?: ReactNode; children: ReactNode },
  { failed: boolean }
> {
  state = { failed: false };

  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true };
  }

  componentDidCatch(error: unknown): void {
    console.error(`[Loading] The ${this.props.name} chunk failed to load`, error);
  }

  render(): ReactNode {
    return this.state.failed ? (this.props.fallback ?? null) : this.props.children;
  }
}

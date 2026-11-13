import type { ComponentType, ReactNode } from "react";
import type { StyleProp, ViewStyle } from "react-native";
import type { ViewportFitOptions, ViewportSize } from "./geometry";

export interface ZoomableViewportAction {
  icon: ComponentType<{ size?: number; color?: string }>;
  label: string;
  onPress: () => void;
  testID?: string;
}

/** What a toolbar of its own needs from the viewport. */
export interface ZoomableViewportControls {
  scale: number;
  minScale: number;
  maxScale: number;
  zoomIn: () => void;
  zoomOut: () => void;
  reset: () => void;
}

export interface ZoomableViewportProps {
  contentSize: ViewportSize;
  children: ReactNode;
  actions?: ZoomableViewportAction[];
  /** Draws the controls in place of the viewport's own toolbar (the image viewer's zoom pill). */
  renderToolbar?: (controls: ZoomableViewportControls) => ReactNode;
  accessibilityLabel?: string;
  fit?: ViewportFitOptions;
  maxScale?: number;
  minScale?: number;
  onPressOutsideContent?: () => void;
  style?: StyleProp<ViewStyle>;
  testID?: string;
  wheelActivation?: "always" | "modifier";
}

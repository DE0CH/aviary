import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { Image, View, type StyleProp, type ViewStyle } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { isWeb } from "@/constants/platform";
import { provisionalContentSize, type ViewportFitOptions, type ViewportSize } from "./geometry";
import { ZoomableViewport } from "./index";
import type { ZoomableViewportAction, ZoomableViewportControls } from "./types";

interface ZoomableImageProps {
  uri: string;
  /** A small rendition of the image: drawn at once, the full image fades in over it. */
  placeholderUri?: string | null;
  accessibilityLabel?: string;
  actions?: ZoomableViewportAction[];
  contentSize?: ViewportSize;
  fit?: ViewportFitOptions;
  maxScale?: number;
  minScale?: number;
  onError?: () => void;
  /** The full image is on screen. */
  onLoad?: () => void;
  onPressOutsideContent?: () => void;
  renderToolbar?: (controls: ZoomableViewportControls) => ReactNode;
  style?: StyleProp<ViewStyle>;
  testID?: string;
  wheelActivation?: "always" | "modifier";
}

/** An image's pixel size, once known; null before, and for no address. */
function useImageSize(uri: string | null | undefined, onError?: () => void): ViewportSize | null {
  const [size, setSize] = useState<ViewportSize | null>(null);
  useEffect(() => {
    setSize(null);
    if (!uri) return undefined;
    let active = true;
    Image.getSize(
      uri,
      (width, height) => {
        if (active && width > 0 && height > 0) setSize({ width, height });
      },
      () => {
        if (active) onError?.();
      },
    );
    return () => {
      active = false;
    };
  }, [onError, uri]);
  return size;
}

/**
 * An image that zooms and pans. With a small rendition (`placeholderUri`) it is laid out and shown from that at
 * once, at the fit the full image will have, and the full image fades in over it when it has come: nothing waits
 * for the full download, and nothing moves when it lands.
 */
export function ZoomableImage({
  uri,
  placeholderUri,
  accessibilityLabel,
  actions,
  contentSize,
  fit,
  maxScale,
  minScale = 1,
  onError,
  onLoad,
  onPressOutsideContent,
  renderToolbar,
  style,
  testID = "zoomable-image",
  wheelActivation = "always",
}: ZoomableImageProps) {
  const fullSize = useImageSize(contentSize ? null : uri, onError);
  const smallSize = useImageSize(contentSize || !placeholderUri ? null : placeholderUri);
  const [loaded, setLoaded] = useState(false);
  useEffect(() => setLoaded(false), [uri]);
  const handleLoad = useCallback(() => {
    setLoaded(true);
    onLoad?.();
  }, [onLoad]);
  const resolvedSize =
    contentSize ?? fullSize ?? (smallSize ? provisionalContentSize(smallSize) : null);
  const source = useMemo(() => ({ uri }), [uri]);
  const placeholder = useMemo(
    () => (placeholderUri ? { uri: placeholderUri } : null),
    [placeholderUri],
  );
  const fullStyle = loaded || !placeholder ? styles.full : styles.fullWaiting;

  const layers = (
    <View style={styles.layers}>
      {placeholder ? (
        <Image
          resizeMode="contain"
          source={placeholder}
          style={styles.image}
          testID={`${testID}-placeholder`}
        />
      ) : null}
      <Image
        accessibilityLabel={resolvedSize ? undefined : accessibilityLabel}
        accessibilityRole={resolvedSize ? undefined : "image"}
        onError={onError}
        onLoad={handleLoad}
        resizeMode="contain"
        source={source}
        style={fullStyle}
        testID={`${testID}-image`}
      />
    </View>
  );

  if (!resolvedSize) {
    // no size yet (no small rendition, the full image still coming): the controls are there all the same, idle
    return (
      <View style={[styles.root, style]} testID={testID}>
        {layers}
        {renderToolbar?.(IDLE_CONTROLS)}
      </View>
    );
  }

  return (
    <ZoomableViewport
      accessibilityLabel={accessibilityLabel}
      actions={actions}
      contentSize={resolvedSize}
      fit={fit}
      maxScale={maxScale}
      minScale={minScale}
      onPressOutsideContent={onPressOutsideContent}
      renderToolbar={renderToolbar}
      style={style}
      testID={testID}
      wheelActivation={wheelActivation}
    >
      {layers}
    </ZoomableViewport>
  );
}

const noop = () => {};
/** The controls before the picture can be zoomed: all idle. */
const IDLE_CONTROLS: ZoomableViewportControls = {
  scale: 1,
  minScale: 1,
  maxScale: 1,
  zoomIn: noop,
  zoomOut: noop,
  reset: noop,
};

// web: the full image fades in; native shows it when it has loaded
const FADE = isWeb ? { transitionProperty: "opacity", transitionDuration: "180ms" } : null;

const styles = StyleSheet.create(() => ({
  root: { flex: 1, minHeight: 0 },
  layers: { width: "100%", height: "100%" },
  image: { position: "absolute", top: 0, left: 0, width: "100%", height: "100%" },
  // the full image over its small rendition: unseen until it has loaded, then faded in
  full: {
    position: "absolute",
    top: 0,
    left: 0,
    width: "100%",
    height: "100%",
    opacity: 1,
    ...FADE,
  },
  fullWaiting: {
    position: "absolute",
    top: 0,
    left: 0,
    width: "100%",
    height: "100%",
    opacity: 0,
    ...FADE,
  },
}));

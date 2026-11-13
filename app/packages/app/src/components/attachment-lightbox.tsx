import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Animated, Easing, Modal, Pressable, Text, View } from "react-native";
import { GestureHandlerRootView } from "react-native-gesture-handler";
import { StyleSheet } from "react-native-unistyles";
import { useSafeAreaInsets } from "react-native-safe-area-context";
import { Minus, Plus, X } from "lucide-react-native";
import { useTranslation } from "react-i18next";
import type { AttachmentMetadata } from "@/attachments/types";
import { useAttachmentPreviewUrl } from "@/attachments/use-attachment-preview-url";
import { isNative, isWeb } from "@/constants/platform";
import { SPACING } from "@/styles/theme";
import { WindowChromeRootRegion } from "@/utils/desktop-window";
import { ZoomableImage } from "@/components/zoomable-viewport/image";
import type { ViewportSize } from "@/components/zoomable-viewport/geometry";
import type { ZoomableViewportControls } from "@/components/zoomable-viewport/types";
import { useGlobalWebOverlayLayer, useWebOverlayRegistration } from "@/lib/overlay-root";

export type ImageLightboxSource =
  | { type: "attachment"; metadata: AttachmentMetadata }
  | { type: "uri"; uri: string; contentSize?: ViewportSize };

export interface AttachmentLightboxProps {
  source: ImageLightboxSource | null;
  onClose: () => void;
}

const ModalRoot = isNative ? GestureHandlerRootView : View;
const LIGHTBOX_FIT = { padding: SPACING[4], maxWidth: 960, maxHeight: 640 };

// The viewer is always on its own dark backdrop, in light and dark themes alike: its controls have their own colours.
const CHROME = {
  fill: "rgba(24, 24, 27, 0.78)",
  fillHover: "rgba(52, 52, 58, 0.9)",
  line: "rgba(255, 255, 255, 0.16)",
  ink: "rgba(255, 255, 255, 0.94)",
  inkIdle: "rgba(255, 255, 255, 0.34)",
};
const CONTROL = 44;
/** How long an attachment's address may take to resolve before the viewer says it could not load the image. */
const ADDRESS_WAIT_MS = 1500;

/** A round control of the viewer (44 px: a finger's target). */
function ChromeButton({
  icon: Icon,
  label,
  onPress,
  disabled = false,
  testID,
}: {
  icon: typeof X;
  label: string;
  onPress: () => void;
  disabled?: boolean;
  testID?: string;
}) {
  const style = useCallback(
    ({ hovered, pressed }: { hovered?: boolean; pressed: boolean }) => [
      styles.chromeButton,
      (hovered || pressed) && !disabled ? styles.chromeButtonActive : null,
    ],
    [disabled],
  );
  return (
    <Pressable
      accessibilityLabel={label}
      accessibilityRole="button"
      disabled={disabled}
      onPress={onPress}
      style={style}
      testID={testID}
    >
      <Icon size={20} color={disabled ? CHROME.inkIdle : CHROME.ink} />
    </Pressable>
  );
}

/** The zoom control: one pill at the bottom, minus, the zoom as a percentage (a tap fits the image again), plus. */
function ZoomPill({ controls }: { controls: ZoomableViewportControls }) {
  const { t } = useTranslation();
  const { scale, minScale, maxScale } = controls;
  const idle = minScale === maxScale;
  return (
    <View pointerEvents="box-none" style={styles.zoomDock}>
      <View style={styles.zoomPill} testID="attachment-lightbox-zoom">
        <ChromeButton
          icon={Minus}
          label={t("message.diagram.zoomOut")}
          onPress={controls.zoomOut}
          disabled={idle || scale <= minScale}
          testID="attachment-lightbox-zoom-out"
        />
        <Pressable
          accessibilityLabel={t("message.diagram.resetZoom")}
          accessibilityRole="button"
          disabled={idle || scale === 1}
          onPress={controls.reset}
          style={styles.zoomValue}
          testID="attachment-lightbox-zoom-reset"
        >
          <Text style={idle ? styles.zoomTextIdle : styles.zoomText}>{Math.round(scale * 100)}%</Text>
        </Pressable>
        <ChromeButton
          icon={Plus}
          label={t("message.diagram.zoomIn")}
          onPress={controls.zoomIn}
          disabled={idle || scale >= maxScale}
          testID="attachment-lightbox-zoom-in"
        />
      </View>
    </View>
  );
}

/** A slim bar along the top while the full image is still coming. */
function LoadingBar() {
  const along = useRef(new Animated.Value(0)).current;
  useEffect(() => {
    const loop = Animated.loop(
      Animated.timing(along, {
        toValue: 1,
        duration: 1100,
        easing: Easing.inOut(Easing.quad),
        useNativeDriver: false,
      }),
    );
    loop.start();
    return () => loop.stop();
  }, [along]);
  const runner = useMemo(
    () => [
      styles.loadingRunner,
      { left: along.interpolate({ inputRange: [0, 1], outputRange: ["-30%", "100%"] }) },
    ],
    [along],
  );
  return (
    <View
      accessibilityRole="progressbar"
      pointerEvents="none"
      style={styles.loadingTrack}
      testID="attachment-lightbox-loading"
    >
      <Animated.View style={runner} />
    </View>
  );
}

/**
 * The image viewer (Deyao, 2026-11-13). Everything of it but the full image is there the moment it opens: the
 * backdrop, the close button, the zoom pill, and the image's small rendition when the server sent one, at the size
 * the image will have. The full image fades in over that; a slim bar says it is still coming.
 */
export function AttachmentLightbox({ source, onClose }: AttachmentLightboxProps) {
  const { t } = useTranslation();
  const insets = useSafeAreaInsets();
  const metadata = source?.type === "attachment" ? source.metadata : null;
  const attachmentUrl = useAttachmentPreviewUrl(metadata);
  const url = source?.type === "uri" ? source.uri : attachmentUrl;
  const contentSize = source?.type === "uri" ? source.contentSize : undefined;
  const [errored, setErrored] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const modalLayer = useGlobalWebOverlayLayer("modal", isWeb && source !== null);

  useEffect(() => {
    setErrored(false);
    setLoaded(false);
  }, [metadata?.id, url]);

  const handleWebOverlayKeyDown = useCallback(
    (event: KeyboardEvent) => {
      if (event.key !== "Escape") return false;
      event.preventDefault();
      event.stopPropagation();
      onClose();
      return true;
    },
    [onClose],
  );
  const setWebOverlayScope = useWebOverlayRegistration({
    active: isWeb && source !== null,
    layer: modalLayer,
    onKeyDown: handleWebOverlayKeyDown,
  });

  const contentLayerStyle = useMemo(
    () => [
      styles.contentLayer,
      {
        paddingTop: insets.top,
        paddingRight: insets.right,
        paddingBottom: insets.bottom,
        paddingLeft: insets.left,
      },
    ],
    [insets.bottom, insets.left, insets.right, insets.top],
  );
  // An attachment's address is resolved a moment after the viewer opens; one that never comes is a failure.
  useEffect(() => {
    if (!source || url) return undefined;
    const timer = setTimeout(() => setErrored(true), ADDRESS_WAIT_MS);
    return () => clearTimeout(timer);
  }, [source, url]);
  const handleImageError = useCallback(() => setErrored(true), []);
  const handleImageLoad = useCallback(() => setLoaded(true), []);
  const renderZoom = useCallback(
    (controls: ZoomableViewportControls) => <ZoomPill controls={controls} />,
    [],
  );

  if (!source) {
    return null;
  }

  return (
    <Modal transparent animationType="fade" statusBarTranslucent visible onRequestClose={onClose}>
      <ModalRoot style={styles.root}>
        <WindowChromeRootRegion corners="both">
          <View ref={setWebOverlayScope} style={styles.root}>
            <Pressable
              testID="attachment-lightbox-backdrop"
              accessibilityRole="button"
              accessibilityLabel={t("message.attachments.dismissImage")}
              onPress={onClose}
              style={styles.backdrop}
            />
            <View pointerEvents="box-none" style={contentLayerStyle}>
              <View pointerEvents="box-none" style={styles.imageArea}>
                {errored ? (
                  <Text style={styles.errorText}>{t("message.attachments.imageLoadFailed")}</Text>
                ) : url ? (
                  <ZoomableImage
                    accessibilityLabel={t("composer.attachments.openImage")}
                    contentSize={contentSize}
                    fit={LIGHTBOX_FIT}
                    onError={handleImageError}
                    onLoad={handleImageLoad}
                    onPressOutsideContent={onClose}
                    placeholderUri={metadata?.previewKey}
                    renderToolbar={renderZoom}
                    style={styles.imageViewport}
                    testID="attachment-lightbox"
                    uri={url}
                  />
                ) : null}
              </View>
              {!loaded && !errored ? <LoadingBar /> : null}
              <View pointerEvents="box-none" style={styles.closeDock}>
                <ChromeButton
                  icon={X}
                  label={t("message.attachments.closeImage")}
                  onPress={onClose}
                  testID="attachment-lightbox-close"
                />
              </View>
            </View>
          </View>
        </WindowChromeRootRegion>
      </ModalRoot>
    </Modal>
  );
}

const styles = StyleSheet.create((theme) => ({
  root: {
    flex: 1,
    minHeight: 0,
    minWidth: 0,
  },
  backdrop: {
    position: "absolute",
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    backgroundColor: "rgba(0,0,0,0.92)",
  },
  contentLayer: {
    position: "absolute",
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
  },
  imageArea: {
    flex: 1,
    alignItems: "center",
    justifyContent: "center",
  },
  imageViewport: {
    flex: 1,
    width: "100%",
    alignSelf: "center",
  },
  errorText: {
    color: CHROME.ink,
    fontSize: theme.fontSize.base,
  },
  closeDock: {
    position: "absolute",
    top: theme.spacing[3],
    right: theme.spacing[3],
  },
  chromeButton: {
    width: CONTROL,
    height: CONTROL,
    borderRadius: CONTROL / 2,
    alignItems: "center",
    justifyContent: "center",
    backgroundColor: CHROME.fill,
    borderWidth: 1,
    borderColor: CHROME.line,
  },
  chromeButtonActive: {
    backgroundColor: CHROME.fillHover,
  },
  zoomDock: {
    position: "absolute",
    left: 0,
    right: 0,
    bottom: theme.spacing[4],
    alignItems: "center",
  },
  // one pill: its two buttons are round on their own, so the pill is their track
  zoomPill: {
    flexDirection: "row",
    alignItems: "center",
    height: CONTROL + 8,
    paddingHorizontal: 4,
    gap: 2,
    borderRadius: (CONTROL + 8) / 2,
    backgroundColor: CHROME.fill,
    borderWidth: 1,
    borderColor: CHROME.line,
  },
  zoomValue: {
    minWidth: 64,
    height: CONTROL,
    alignItems: "center",
    justifyContent: "center",
  },
  zoomText: {
    color: CHROME.ink,
    fontSize: theme.fontSize.base,
    fontVariant: ["tabular-nums"],
  },
  zoomTextIdle: {
    color: CHROME.inkIdle,
    fontSize: theme.fontSize.base,
    fontVariant: ["tabular-nums"],
  },
  loadingTrack: {
    position: "absolute",
    top: 0,
    left: 0,
    right: 0,
    height: 2,
    overflow: "hidden",
    backgroundColor: "rgba(255, 255, 255, 0.08)",
  },
  loadingRunner: {
    position: "absolute",
    top: 0,
    bottom: 0,
    width: "30%",
    backgroundColor: CHROME.ink,
  },
}));

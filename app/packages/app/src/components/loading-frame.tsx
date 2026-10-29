import { Component, type ReactNode } from "react";
import { Text, View, useWindowDimensions, type StyleProp, type ViewStyle } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { RotateCw } from "lucide-react-native";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";
import { isWeb } from "@/constants/platform";
import { isChunkLoadError } from "@/utils/lazy-with-retry";

/**
 * What a lazy area shows while its chunk is on the way: an empty area of the app's own background. It carries
 * data-yyc-loading, which tells public/index.html's early message box that the app is still arriving
 * (docs/paseo-ui.md "Loading").
 */
export function LoadingFrame({ style, children }: { style?: StyleProp<ViewStyle>; children?: ReactNode }) {
  return (
    <View dataSet={{ yycLoading: "" }} style={[{ flex: 1 }, style]}>
      {children}
    </View>
  );
}

// The composer's slot (composer/index.tsx: 16 px around it, at most theme.contentMaxWidth wide; public/index.html's
// frame: 116 px tall, 104 px below 720 px). data-yyc-box is what the early message box's place() measures.
const COMPOSER_SLOT_DATASET = { yycBox: "" };

/**
 * A box at the bottom of the main column where the composer will be, so the message box never leaves the page while
 * the composer is on the way. `hidden` draws nothing but still holds the place (for the early box to sit on).
 */
export function ComposerSlot({ hidden = false }: { hidden?: boolean }) {
  const { width } = useWindowDimensions();
  return (
    <View style={styles.slotRow} pointerEvents="none" aria-hidden>
      <View
        dataSet={COMPOSER_SLOT_DATASET}
        style={[styles.slot, { height: width < 720 ? 104 : 116 }, hidden && styles.slotHidden]}
      />
    </View>
  );
}

/** The conversation page while its chunk is on the way: a loading frame with the message box at the bottom. */
export function WorkspaceLoadingFrame() {
  return (
    <LoadingFrame style={styles.workspace}>
      <ComposerSlot />
    </LoadingFrame>
  );
}

/**
 * A lazy area whose chunk could not be fetched even after lazyWithRetry's retries: one line and a Reload button.
 * Reloading the page is the only way past React.lazy's cached failure.
 */
export function ChunkLoadFailed() {
  const { t } = useTranslation();
  return (
    <View style={styles.failed} testID="chunk-load-failed">
      <Text style={styles.failedText}>{t("common.pageLoadFailed")}</Text>
      {isWeb ? (
        <Button variant="ghost" size="xs" leftIcon={RotateCw} onPress={() => window.location.reload()}>
          {t("common.actions.reload")}
        </Button>
      ) : null}
    </View>
  );
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

const styles = StyleSheet.create((theme) => ({
  workspace: { justifyContent: "flex-end" },
  slotRow: { width: "100%", paddingHorizontal: theme.spacing[4], paddingBottom: theme.spacing[4], alignItems: "center" },
  slot: {
    width: "100%",
    maxWidth: theme.contentMaxWidth,
    borderWidth: 1,
    borderColor: theme.colors.border,
    borderRadius: theme.borderRadius["2xl"],
    backgroundColor: theme.colors.surface1,
  },
  slotHidden: { opacity: 0 },
  failed: {
    flex: 1,
    alignItems: "center",
    justifyContent: "center",
    gap: theme.spacing[3],
    paddingHorizontal: theme.spacing[4],
    backgroundColor: theme.colors.surface0,
  },
  failedText: { fontSize: theme.fontSize.sm, color: theme.colors.foregroundMuted, textAlign: "center" },
}));

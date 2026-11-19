import { useMemo } from "react";
import { Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import type { Theme } from "@/styles/theme";

/**
 * How far an attachment's upload has come: a bar along the chip's foot, filled to the share sent, and the share said
 * over the body when the chip has no text of its own (a picture's thumbnail).
 */
export function UploadOverlay({ fraction, label }: { fraction: number; label?: string }) {
  const percent = Math.round(Math.min(1, Math.max(0, fraction)) * 100);
  const fillStyle = useMemo(() => [styles.uploadFill, { width: `${percent}%` as const }], [percent]);
  return (
    <View style={styles.uploadScrim} pointerEvents="none">
      {label && fraction < 1 ? (
        <View style={styles.uploadLabelWrap}>
          <Text style={styles.uploadLabelText} numberOfLines={1}>
            {label}
          </Text>
        </View>
      ) : null}
      <View
        testID="attachment-upload-progress"
        accessibilityRole="progressbar"
        accessibilityValue={{ min: 0, max: 100, now: percent }}
        style={styles.uploadTrack}
      >
        <View style={fillStyle} />
      </View>
    </View>
  );
}

const styles = StyleSheet.create((theme: Theme) => ({
  uploadScrim: {
    position: "absolute",
    left: 0,
    right: 0,
    top: 0,
    bottom: 0,
    justifyContent: "center",
    alignItems: "center",
  },
  uploadLabelWrap: {
    paddingHorizontal: theme.spacing[1],
    paddingVertical: 1,
    borderRadius: theme.borderRadius.sm,
    backgroundColor: theme.colors.surface1,
    opacity: 0.9,
  },
  uploadLabelText: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  uploadTrack: {
    position: "absolute",
    left: 0,
    right: 0,
    bottom: 0,
    height: 3,
    backgroundColor: theme.colors.surface2,
  },
  uploadFill: {
    height: 3,
    backgroundColor: theme.colors.accent,
  },
}));

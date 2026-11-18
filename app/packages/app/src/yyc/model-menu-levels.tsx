import { useCallback, useRef } from "react";
import { Pressable, Text, View, type LayoutChangeEvent } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import {
  LEVEL_CELL_BORDER,
  LEVEL_TRACK_MARGIN,
  LEVEL_TRACK_PADDING,
} from "@/yyc/model-menu-state";

/**
 * Every level's label drawn once where nobody sees it, reporting the widest as it renders (`onWidest`): the model
 * menu's width follows it (model-menu-state `menuWidth`), so the cells fit the labels in any language and font. It
 * sits beside the menu's button, which is always there, so the width is known before the menu opens.
 */
export function LevelLabelProbe({ labels, onWidest }: { labels: string[]; onWidest: (width: number) => void }) {
  const widths = useRef(new Map<string, number>());
  const report = useCallback(() => {
    let widest = 0;
    for (const label of labels) {
      const width = widths.current.get(label);
      if (width === undefined) return;
      widest = Math.max(widest, width);
    }
    onWidest(widest);
  }, [labels, onWidest]);
  return (
    <View aria-hidden pointerEvents="none" style={styles.probe} testID="yyc-model-level-probe">
      {labels.map((label) => (
        <Text
          key={label}
          style={[styles.levelText, styles.probeText]}
          onLayout={(event: LayoutChangeEvent) => {
            widths.current.set(label, event.nativeEvent.layout.width);
            report();
          }}
        >
          {label}
        </Text>
      ))}
    </View>
  );
}

/**
 * The thinking row: one equal cell per level on a track, the picked one raised; a level the model lacks stays in its
 * place, dimmed and not pressable.
 */
export function ThinkingLevels({
  row,
  offered,
  picked,
  label,
  onPick,
}: {
  row: string[];
  offered: string[];
  picked: string | null;
  label: (level: string) => string;
  onPick: (level: string) => void;
}) {
  return (
    <View style={styles.levels} testID="yyc-model-levels">
      {row.map((level) => {
        const has = offered.includes(level);
        const isPicked = level === picked;
        return (
          <Pressable
            key={level}
            testID={`yyc-model-level-${level}`}
            accessibilityRole="button"
            accessibilityState={{ selected: isPicked, disabled: !has }}
            disabled={!has}
            onPress={() => onPick(level)}
            style={[styles.levelCell, isPicked && styles.levelPicked, !has && styles.levelMissing]}
          >
            <Text style={isPicked ? styles.levelTextPicked : styles.levelText} numberOfLines={1}>
              {label(level)}
            </Text>
          </Pressable>
        );
      })}
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  // unseen and out of the flow; its labels at their own width (not stretched to the box)
  probe: {
    position: "absolute",
    left: 0,
    top: 0,
    opacity: 0,
    alignItems: "flex-start",
  },
  probeText: {
    flexShrink: 0,
  },
  // the track: its margin and padding are the numbers menuWidth adds, so the two cannot drift apart
  levels: {
    flexDirection: "row",
    marginHorizontal: LEVEL_TRACK_MARGIN,
    marginBottom: theme.spacing[2],
    padding: LEVEL_TRACK_PADDING,
    borderRadius: theme.borderRadius.lg,
    backgroundColor: theme.colors.surface2,
  },
  levelCell: {
    flex: 1,
    minHeight: 32,
    alignItems: "center",
    justifyContent: "center",
    borderRadius: theme.borderRadius.md,
    borderWidth: LEVEL_CELL_BORDER,
    borderColor: "transparent",
  },
  levelPicked: {
    backgroundColor: theme.colors.surface0,
    borderColor: theme.colors.border,
  },
  levelMissing: {
    opacity: 0.35,
  },
  levelText: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  levelTextPicked: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
}));

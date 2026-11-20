import { forwardRef, useMemo } from "react";
import { Pressable, StyleSheet, View } from "react-native";
import { Gesture, GestureDetector } from "react-native-gesture-handler";
import Animated, { useAnimatedStyle, useSharedValue } from "react-native-reanimated";
import { resolveHighlightCorners } from "./press-highlight.shape";
import type { PressHighlightProps } from "./press-highlight.types";

const HELD_MS = 60_000;

export const PressHighlight = forwardRef<View, PressHighlightProps>(function PressHighlight(
  { children, disabled, highlightStyle, ...props },
  ref,
) {
  const highlighted = useSharedValue(0);
  const corners = useMemo(() => resolveHighlightCorners(props.style), [props.style]);
  const animatedHighlightStyle = useAnimatedStyle(() => ({ opacity: highlighted.value }));
  // This gesture owns visual acknowledgement only. Pressable remains the sole owner of
  // activation, cancellation, accessibility, and the caller's drag/long-press callbacks.
  const pressGesture = useMemo(
    () =>
      Gesture.Tap()
        .enabled(disabled !== true && highlightStyle != null)
        .maxDistance(8)
        // the highlight stays while the finger does (a tap's default 500 ms ended it under a held finger, before a
        // long press's menu; Deyao, 2026-11-20); activation stays the Pressable's
        .maxDuration(HELD_MS)
        .shouldCancelWhenOutside(true)
        .onBegin(() => {
          highlighted.value = 1;
        })
        .onFinalize(() => {
          highlighted.value = 0;
        }),
    [disabled, highlightStyle, highlighted],
  );

  const highlight = highlightStyle ? (
    <Animated.View pointerEvents="none" style={[styles.highlight, animatedHighlightStyle]}>
      {/* Keep the themed Unistyles node separate from the node Reanimated patches. The
          pressable's corners come first so `highlightStyle` can still override them. */}
      <View style={[styles.highlightFill, corners, highlightStyle]} />
    </Animated.View>
  ) : null;
  const renderedChildren =
    typeof children === "function" ? (
      (state: Parameters<typeof children>[0]) => (
        <>
          {highlight}
          {children(state)}
        </>
      )
    ) : (
      <>
        {highlight}
        {children}
      </>
    );

  const pressable = (
    <Pressable ref={ref} {...props} collapsable={false} disabled={disabled}>
      {renderedChildren}
    </Pressable>
  );

  if (!highlightStyle) {
    return pressable;
  }

  return <GestureDetector gesture={pressGesture}>{pressable}</GestureDetector>;
});

const styles = StyleSheet.create({
  highlight: {
    ...StyleSheet.absoluteFill,
  },
  highlightFill: {
    ...StyleSheet.absoluteFill,
  },
});

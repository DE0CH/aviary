/**
 * Something at the bottom of the chat on a phone that appears, changes height or goes (Deyao, 2026-11-20, chosen from
 * mocks): its height eases and its content stays pinned to one edge, so nothing jumps. Pinned to the bottom (a card),
 * only its top edge moves; pinned to the top (a reply growing as it streams), new lines rise into view as the list
 * makes room. The decision is ease-height-motion.ts; web has no motion (ease-height.web.tsx).
 *
 * Until its first layout the content is in normal flow, so a row that was already there never shows a blank frame;
 * after it, the content is measured in an absolutely placed wrapper and the clip around it animates its height.
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { View, type LayoutChangeEvent, type StyleProp, type ViewStyle } from "react-native";
import Animated, {
  Easing,
  runOnJS,
  useAnimatedStyle,
  useReducedMotion,
  useSharedValue,
  withSpring,
  withTiming,
} from "react-native-reanimated";
import { heightMotion, SPRINGS, type HeightCurve } from "./ease-height-motion";

export interface EaseHeightProps {
  children: ReactNode;
  /** The edge the content stays on while the height changes. */
  pin?: "top" | "bottom";
  curve?: HeightCurve;
  /** A new item eases in from zero; otherwise its first layout shows at full height. */
  fromZero?: boolean;
  /** Ease out to zero, then call onLeft (the owner drops it). */
  leaving?: boolean;
  onLeft?: () => void;
  style?: StyleProp<ViewStyle>;
  testID?: string;
}

export function EaseHeight({
  children,
  pin = "bottom",
  curve = "ease",
  fromZero = false,
  leaving = false,
  onLeft,
  style,
  testID,
}: EaseHeightProps) {
  const reduceMotion = useReducedMotion();
  const height = useSharedValue(0);
  const [clipped, setClipped] = useState(fromZero);
  const first = useRef(true);

  const go = (to: number, then?: () => void) => {
    const motion = heightMotion({ to, first: first.current, fromZero, curve, reduceMotion });
    first.current = false;
    const done = then
      ? (finished?: boolean) => {
          "worklet";
          if (finished) runOnJS(then)();
        }
      : undefined;
    if (motion.kind === "set") {
      height.value = motion.to;
      then?.();
    } else if (motion.kind === "timing") {
      height.value = withTiming(motion.to, { duration: motion.duration, easing: Easing.out(Easing.cubic) }, done);
    } else {
      height.value = withSpring(motion.to, SPRINGS[motion.spring], done);
    }
  };

  const onLayout = (event: LayoutChangeEvent) => {
    if (leaving) return;
    go(event.nativeEvent.layout.height);
    if (!clipped) setClipped(true);
  };

  useEffect(() => {
    if (leaving) go(0, onLeft);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- runs when it starts leaving
  }, [leaving]);

  const clipStyle = useAnimatedStyle(() => (clipped ? { height: height.value } : {}), [clipped]);
  return (
    <Animated.View style={[clipped ? CLIP : null, style, clipStyle]} testID={testID}>
      <View onLayout={onLayout} style={clipped ? (pin === "top" ? PINNED_TOP : PINNED_BOTTOM) : null}>
        {children}
      </View>
    </Animated.View>
  );
}

const CLIP = { overflow: "hidden" } as const;
const PINNED_TOP = { position: "absolute", left: 0, right: 0, top: 0 } as const;
const PINNED_BOTTOM = { position: "absolute", left: 0, right: 0, bottom: 0 } as const;

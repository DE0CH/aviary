/** Web has no motion (desktop stays as it was; Deyao, 2026-11-20): the children as they are, and a leaving item goes at once. */
import { useEffect } from "react";
import { View } from "react-native";
import type { EaseHeightProps } from "./ease-height";

export type { EaseHeightProps } from "./ease-height";

export function EaseHeight({ children, leaving = false, onLeft, style, testID }: EaseHeightProps) {
  useEffect(() => {
    if (leaving) onLeft?.();
  }, [leaving, onLeft]);
  if (!style && !testID) return <>{children}</>;
  return (
    <View style={style} testID={testID}>
      {children}
    </View>
  );
}

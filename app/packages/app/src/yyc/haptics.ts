/**
 * The phone's taps for the chat (docs/paseo-ui.md "The iOS app"): a light one when a message goes out, a firmer one
 * when the reply is stopped, and the system's "done" when a reply ends while its conversation is on screen.
 * Nothing on the web.
 */
import * as Haptics from "expo-haptics";
import { isNative } from "@/constants/platform";

export type ChatHaptic = "send" | "stop" | "reply-ended";

export function playChatHaptic(event: ChatHaptic): void {
  if (!isNative) return;
  let played: Promise<void>;
  if (event === "send") played = Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Light);
  else if (event === "stop") played = Haptics.impactAsync(Haptics.ImpactFeedbackStyle.Medium);
  else played = Haptics.notificationAsync(Haptics.NotificationFeedbackType.Success);
  // a phone without a Taptic Engine, or Low Power Mode: no tap, no error
  void played.catch(() => {});
}

/** A reply ended: it was running at the last look and is not now. */
export function replyEnded(wasRunning: boolean, isRunning: boolean): boolean {
  return wasRunning && !isRunning;
}

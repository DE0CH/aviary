import type { RevokePushNotificationsInput, StartPushNotificationsInput } from "./internal/types";

export function startPushNotifications(_input: StartPushNotificationsInput): () => void {
  return () => undefined;
}

export async function revokePushNotifications(_input: RevokePushNotificationsInput): Promise<void> {
  // CloudCLI's server sends no push notifications (docs/paseo-ui.md): no permission prompt, no token.
}

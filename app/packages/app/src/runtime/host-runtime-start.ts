import { shouldUseDesktopDaemon } from "@/desktop/daemon/desktop-daemon";
import { loadDesktopSettings } from "@/desktop/settings/desktop-settings";
import { startHostRuntimeBootstrap } from "@/navigation/host-runtime-bootstrap";
import { getDaemonStartService } from "@/runtime/daemon-start-service";
import { getHostRuntimeStore, hasConfiguredLocalDaemonOverride } from "@/runtime/host-runtime";

export async function shouldStartBuiltInDaemon(): Promise<boolean> {
  if (!shouldUseDesktopDaemon()) {
    return false;
  }
  if (hasConfiguredLocalDaemonOverride()) {
    return false;
  }
  const settings = await loadDesktopSettings();
  return settings.daemon.manageBuiltInDaemon;
}

let hostRuntimeBootstrapStarted = false;

/** Starts the connection once: on web from index.ts when the entry runs, on native from app/_layout.tsx. */
export function startHostRuntimeBootstrapOnce(): void {
  if (hostRuntimeBootstrapStarted) return;
  hostRuntimeBootstrapStarted = true;
  const store = getHostRuntimeStore();
  const daemonStartService = getDaemonStartService({ store });
  startHostRuntimeBootstrap({
    store,
    daemonStartService,
    shouldStartDaemon: shouldStartBuiltInDaemon,
  });
}

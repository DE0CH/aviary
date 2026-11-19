import { useEffect, useState, type ReactElement } from "react";
import { lazyArea } from "@/components/lazy-area";
import type { AttachmentLightboxProps } from "@/components/attachment-lightbox";

const loadLightbox = () => import("@/components/attachment-lightbox");

// The image viewer (zoom and pan) is its own chunk (docs/paseo-ui.md "Loading"): not part of the first load, but
// fetched as soon as a conversation that can open it is on screen and the page is idle, so the first tap on an
// image opens the viewer at once (Deyao, 2026-11-13: its controls came late). From then on it stays mounted, so it
// can animate closed.
const Lightbox = lazyArea("attachment-lightbox", loadLightbox, (m) => m.AttachmentLightbox, null);

let warmed = false;
function warmLightbox(): void {
  if (warmed) return;
  warmed = true;
  const fetchIt = () => void loadLightbox().catch(() => (warmed = false));
  if (typeof requestIdleCallback === "function") requestIdleCallback(fetchIt, { timeout: 4000 });
  else setTimeout(fetchIt, 1500);
}

export function AttachmentLightbox(props: AttachmentLightboxProps): ReactElement | null {
  const [opened, setOpened] = useState(props.source !== null);
  if (props.source !== null && !opened) setOpened(true);
  useEffect(warmLightbox, []);
  return opened || props.source !== null ? <Lightbox {...props} /> : null;
}

import { useState, type ReactElement } from "react";
import { lazyArea } from "@/components/lazy-area";
import type { AttachmentLightboxProps } from "@/components/attachment-lightbox";

// The image viewer (zoom and pan) is its own chunk, fetched the first time an image opens (docs/paseo-ui.md
// "Loading"); from then on it stays mounted, so it can animate closed.
const Lightbox = lazyArea(
  "attachment-lightbox",
  () => import("@/components/attachment-lightbox"),
  (m) => m.AttachmentLightbox,
  null
);

export function AttachmentLightbox(props: AttachmentLightboxProps): ReactElement | null {
  const [opened, setOpened] = useState(props.source !== null);
  if (props.source !== null && !opened) setOpened(true);
  return opened || props.source !== null ? <Lightbox {...props} /> : null;
}

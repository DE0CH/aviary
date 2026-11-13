import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AttachmentLightbox } from "./attachment-lightbox";

beforeEach(() => vi.stubGlobal("React", React));

const mounted: { root: Root; container: HTMLDivElement }[] = [];
afterEach(() => {
  for (const entry of mounted.splice(0)) {
    act(() => entry.root.unmount());
    entry.container.remove();
  }
});

// Deyao, 2026-11-13: the viewer's close and zoom buttons came late (they waited for the full image to download).
// The viewer's controls are there the moment it opens, whatever the image does.
describe("AttachmentLightbox", () => {
  it("has its close button and zoom controls at once, before the image has loaded", () => {
    const onClose = vi.fn();
    const container = document.createElement("div");
    document.body.appendChild(container);
    const root = createRoot(container);
    // an address that never answers: the image never loads
    const source = { type: "uri" as const, uri: "http://127.0.0.1:9/never.png" };
    act(() => root.render(<AttachmentLightbox source={source} onClose={onClose} />));
    mounted.push({ root, container });

    const close = document.querySelector<HTMLElement>('[data-testid="attachment-lightbox-close"]');
    expect(close).not.toBeNull();
    expect(document.querySelector('[data-testid="attachment-lightbox-zoom"]')).not.toBeNull();
    expect(document.querySelector('[data-testid="attachment-lightbox-loading"]')).not.toBeNull();
    act(() => close?.click());
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

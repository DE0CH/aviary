// The files a turn shows under its answer (docs/CHAT.md, "Presented files"): only what the agent passed to
// present_files, in its order. Stored in turns.presented, sent as `files_presented` events. Shared by the tools role
// (the tool) and the turn runner (the final list once the turn's files are stored).

export interface PresentedFile {
  path: string;
  caption?: string;
  sha256: string;
  size: number;
}

export const MAX_PRESENTED = 20;
export const MAX_CAPTION = 300;

/**
 * Adds `next` to `prev`: a path presented again is updated and moves to its new place (the latest version wins), and
 * the same contents under a second path (a copy, e.g. build/x.pdf and x.pdf) are shown once, as the latest one.
 */
export function mergePresented(prev: PresentedFile[], next: PresentedFile[]): PresentedFile[] {
  const out: PresentedFile[] = [];
  for (const f of [...prev, ...next]) {
    for (let i = out.length - 1; i >= 0; i--) {
      const o = out[i]!;
      if (o.path === f.path || o.sha256 === f.sha256) out.splice(i, 1);
    }
    out.push(f);
  }
  return out.slice(-MAX_PRESENTED);
}

/** A stored list (jsonb) read back defensively. */
export function presentedOf(v: unknown): PresentedFile[] | null {
  if (v == null) return null;
  if (!Array.isArray(v)) return [];
  return v
    .filter((x): x is Record<string, unknown> => !!x && typeof x === "object" && typeof (x as { path?: unknown }).path === "string")
    .map((x) => ({
      path: String(x.path),
      ...(typeof x.caption === "string" && x.caption ? { caption: x.caption } : {}),
      sha256: String(x.sha256 ?? ""),
      size: Number(x.size ?? 0),
    }));
}

/** Browser-viewable images (inline previews); everything else is a plain card (PDFs get a first-page preview). */
export const IMAGE_EXT = /\.(png|jpe?g|gif|webp)$/i;

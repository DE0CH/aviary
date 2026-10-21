// A small PNG of a PDF page for the file cards under an answer (docs/CHAT.md, "Presented files"): poppler's pdftoppm
// (in the web image), at most 480 px wide, kept in memory per blob and page. No pdftoppm (a dev machine) = no preview.
import { execFile } from "node:child_process";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { WebDeps } from "./ctx.ts";

const MAX_PDF_BYTES = 50 * 1024 * 1024;
const CACHE_MAX = 64;
const cache = new Map<string, Promise<Buffer | null>>();
let running = 0;

export function pdfPreview(deps: WebDeps, key: string, page: number): Promise<Buffer | null> {
  const id = `${key}#${page}`;
  const hit = cache.get(id);
  if (hit) {
    cache.delete(id);
    cache.set(id, hit);
    return hit;
  }
  const p = render(deps, key, page).catch(() => null);
  cache.set(id, p);
  void p.then((b) => b || cache.delete(id));
  while (cache.size > CACHE_MAX) cache.delete(cache.keys().next().value!);
  return p;
}

async function render(deps: WebDeps, key: string, page: number): Promise<Buffer | null> {
  if (running >= 2) return null; // previews are a nicety: never queue CPU work behind them
  running++;
  const dir = await mkdtemp(join(tmpdir(), "pdfprev-"));
  try {
    const pdf = await deps.blobs.get(key);
    if (!pdf || pdf.length > MAX_PDF_BYTES) return null;
    await writeFile(join(dir, "in.pdf"), pdf);
    await new Promise<void>((ok, fail) =>
      execFile(
        process.env.JARVIS_PDFTOPPM || "pdftoppm",
        ["-f", String(page), "-l", String(page), "-png", "-singlefile", "-scale-to", "480", join(dir, "in.pdf"), join(dir, "out")],
        { timeout: 15_000 },
        (e) => (e ? fail(e) : ok()),
      ),
    );
    return await readFile(join(dir, "out.png"));
  } finally {
    running--;
    await rm(dir, { recursive: true, force: true });
  }
}

// Builds the staff chat UI into ../ui-dist (served by the web role at /): index.html, app-<hash>.js, app-<hash>.css.
// `node build.mjs --watch` rebuilds on change (for dev/mock-server.mjs).
import { build, context } from "esbuild";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "..", "ui-dist");
const watch = process.argv.includes("--watch");

const options = {
  entryPoints: { app: join(here, "src", "main.tsx") },
  bundle: true,
  format: "esm",
  target: ["es2020", "safari15"],
  jsx: "automatic",
  jsxImportSource: "preact",
  minify: !watch,
  sourcemap: watch ? "inline" : false,
  legalComments: "none",
  write: false,
  outdir: out,
  loader: { ".css": "css" },
  define: { "process.env.NODE_ENV": watch ? '"development"' : '"production"' },
};

function emit(result) {
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  const names = {};
  for (const f of result.outputFiles) {
    const ext = f.path.endsWith(".css") ? "css" : "js";
    const hash = createHash("sha256").update(f.contents).digest("hex").slice(0, 10);
    names[ext] = `app-${hash}.${ext}`;
    writeFileSync(join(out, names[ext]), f.contents);
  }
  const html = readFileSync(join(here, "src", "index.html"), "utf8")
    .replace("%CSS%", names.css ?? "")
    .replace("%JS%", names.js ?? "");
  writeFileSync(join(out, "index.html"), html);
  writeFileSync(join(out, "manifest.webmanifest"), readFileSync(join(here, "src", "manifest.webmanifest")));
  const size = readdirSync(out).map((n) => `${n} ${readFileSync(join(out, n)).length}`);
  console.log(`ui-dist: ${size.join(", ")}`);
}

if (watch) {
  const ctx = await context({ ...options, plugins: [{ name: "emit", setup: (b) => b.onEnd((r) => r.errors.length || emit(r)) }] });
  await ctx.watch();
} else {
  emit(await build(options));
}

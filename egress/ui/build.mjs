// Builds the staff chat UI into ../ui-dist (served by the web role at /): index.html, app-<hash>.js, app-<hash>.css;
// and the admin monitor (served at /admin/* to config admins only): admin.html, admin-<hash>.js, admin-<hash>.css.
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
  entryPoints: { app: join(here, "src", "main.tsx"), admin: join(here, "src", "admin.tsx") },
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
  const names = { app: {}, admin: {} };
  for (const f of result.outputFiles) {
    const ext = f.path.endsWith(".css") ? "css" : "js";
    const entry = /admin\.(js|css)$/.test(f.path) ? "admin" : "app";
    const hash = createHash("sha256").update(f.contents).digest("hex").slice(0, 10);
    names[entry][ext] = `${entry}-${hash}.${ext}`;
    writeFileSync(join(out, names[entry][ext]), f.contents);
  }
  for (const [entry, page] of [["app", "index.html"], ["admin", "admin.html"]]) {
    const html = readFileSync(join(here, "src", page), "utf8")
      .replace("%CSS%", names[entry].css ?? "")
      .replace("%JS%", names[entry].js ?? "");
    writeFileSync(join(out, page), html);
  }
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

/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import esbuild from 'esbuild';

const production = process.argv.includes('--production');
const watch = process.argv.includes('--watch');

/**
 * @type {import('esbuild').Plugin}
 */
const esbuildProblemMatcherPlugin = {
  name: 'esbuild-problem-matcher',

  setup(build) {
    build.onStart(() => {
      console.log('[watch] build started');
    });
    build.onEnd((result) => {
      result.errors.forEach(({ text, location }) => {
        console.error(`✘ [ERROR] ${text}`);
        console.error(
          `    ${location.file}:${location.line}:${location.column}:`,
        );
      });
      console.log('[watch] build finished');
    });
  },
};

/**
 * @type {import('esbuild').Plugin}
 */
const cssInjectPlugin = {
  name: 'css-inject',
  setup(build) {
    // Handle CSS files
    build.onLoad({ filter: /\.css$/ }, async (args) => {
      const fs = await import('fs');
      const css = await fs.promises.readFile(args.path, 'utf8');
      return {
        contents: `
          const style = document.createElement('style');
          style.textContent = ${JSON.stringify(css)};
          document.head.appendChild(style);
        `,
        loader: 'js',
      };
    });

    // Handle SCSS files
    build.onLoad({ filter: /\.scss$/ }, async (args) => {
      const sass = await import('sass');
      const result = sass.compile(args.path, {
        loadPaths: [args.path.substring(0, args.path.lastIndexOf('/'))],
      });
      const css = result.css;
      return {
        contents: `
          const style = document.createElement('style');
          style.textContent = ${JSON.stringify(css)};
          document.head.appendChild(style);
        `,
        loader: 'js',
      };
    });
  },
};

async function main() {
  // Build extension
  const extensionCtx = await esbuild.context({
    entryPoints: ['src/extension.ts'],
    bundle: true,
    format: 'cjs',
    minify: production,
    sourcemap: !production,
    sourcesContent: false,
    platform: 'node',
    outfile: 'dist/extension.cjs',
    external: ['vscode'],
    logLevel: 'silent',
    banner: {
      js: `const import_meta = { url: require('url').pathToFileURL(__filename).href };`,
    },
    define: {
      'import.meta.url': 'import_meta.url',
    },
    plugins: [
      /* add to the end of plugins array */
      esbuildProblemMatcherPlugin,
    ],
    loader: { '.node': 'file' },
  });

  // Build webview
  const webviewCtx = await esbuild.context({
    entryPoints: ['src/webview/index.tsx'],
    bundle: true,
    format: 'iife',
    minify: production,
    sourcemap: !production,
    sourcesContent: false,
    platform: 'browser',
    outfile: 'dist/webview.js',
    logLevel: 'silent',
    plugins: [cssInjectPlugin, esbuildProblemMatcherPlugin],
    jsx: 'automatic', // Use new JSX transform (React 17+)
    define: {
      'process.env.NODE_ENV': production ? '"production"' : '"development"',
    },
  });

  if (watch) {
    await Promise.all([extensionCtx.watch(), webviewCtx.watch()]);
  } else {
    await Promise.all([extensionCtx.rebuild(), webviewCtx.rebuild()]);
    await Promise.all([extensionCtx.dispose(), webviewCtx.dispose()]);
  }
}

main().catch((e) => {
  console.error(e);
  process.exit(1);
});

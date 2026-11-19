import { defineConfig, configDefaults } from "vitest/config";
import { playwright } from "@vitest/browser-playwright";
import path from "path";
import fs from "fs";
import { transformSync } from "@babel/core";
import { build } from "esbuild";

// Set this before Vite resolves dependencies and Vitest starts its workers.
Object.assign(process.env, { NODE_ENV: "test" });

const appNodeModules = path.resolve(__dirname, "node_modules");
const rootNodeModules = path.resolve(__dirname, "../../node_modules");
const resolvePackageEntry = (packageName: string) => {
  const appPackagePath = path.resolve(appNodeModules, packageName);
  return fs.existsSync(appPackagePath)
    ? appPackagePath
    : path.resolve(rootNodeModules, packageName);
};

export default defineConfig({
  plugins: [
    {
      name: "markdown-wasm-commonjs-export",
      transform(source, id) {
        // The ESM package contains a CommonJS WASM factory. Metro handles the
        // interop; Vite needs an ESM export instead of the CommonJS trailer.
        if (!id.endsWith("/lib/module/web/wasm/md4c.js")) return;
        const trailer = source.lastIndexOf("\nif (typeof exports === 'object'");
        if (trailer < 0) throw new Error("Markdown WASM CommonJS trailer was not found");
        return {
          code: `${source.slice(0, trailer)}\nexport default createMd4cModule;`,
          map: null,
        };
      },
    },
    {
      name: "reanimated-web-utils-esm",
      // Reanimated loads react-native-web's style compiler with `require`, which Vite does not
      // have: without the compiler an animated style on a mounted view throws. Metro resolves it.
      transform(_source, id) {
        if (!id.split("?")[0].endsWith("/ReanimatedModule/js-reanimated/webUtils.web.js")) return;
        return {
          code: [
            'import createReactDOMStyle from "react-native-web/dist/exports/StyleSheet/compiler/createReactDOMStyle";',
            'import { createTransformValue, createTextShadowValue } from "react-native-web/dist/exports/StyleSheet/preprocess";',
            "export { createReactDOMStyle, createTransformValue, createTextShadowValue };",
          ].join("\n"),
          map: null,
        };
      },
    },
    {
      name: "compile-usage-components",
      enforce: "pre",
      transform(source, id) {
        // These labels must react to time and language in the compiled app too.
        if (!id.includes("/src/usage/") || !id.endsWith(".tsx") || id.includes(".test.")) return;
        const result = transformSync(source, {
          filename: id,
          babelrc: false,
          configFile: false,
          parserOpts: { plugins: ["typescript", "jsx"] },
          plugins: [["babel-plugin-react-compiler", { target: "19" }]],
          sourceMaps: true,
        });
        if (result?.code) return { code: result.code, map: result.map };
      },
    },
    {
      name: "bundle-i18n-resources",
      // Metro resolves literal requires lazily. Vite's TS transform leaves them
      // to Node, which cannot resolve the resource modules. Bundle this boundary
      // so unit and browser consumers exercise the same deferred module loading.
      async transform(_source, id) {
        if (path.resolve(id) !== path.resolve(__dirname, "src/i18n/i18next.ts")) return;
        const result = await build({
          entryPoints: [id],
          bundle: true,
          format: "esm",
          platform: "neutral",
          external: ["i18next", "react-i18next"],
          write: false,
        });
        return { code: result.outputFiles[0].text, map: null };
      },
    },
  ],
  test: {
    environment: "node",
    exclude: [...configDefaults.exclude, "e2e/**"],
    projects: [
      {
        extends: true,
        test: {
          name: "unit",
          environment: "node",
          include: [
            "src/**/*.{test,spec}.{ts,tsx}",
            "native-release-version.test.ts",
            "app.config.test.ts",
            "vitest-environment.test.ts",
          ],
          setupFiles: [path.resolve(__dirname, "vitest.setup.ts")],
          exclude: [...configDefaults.exclude, "e2e/**", "src/**/*.browser.{test,spec}.{ts,tsx}"],
        },
      },
      {
        extends: true,
        test: {
          name: "browser",
          fileParallelism: false,
          include: ["src/**/*.browser.{test,spec}.{ts,tsx}"],
          browser: {
            enabled: true,
            provider: playwright(),
            headless: true,
            connectTimeout: 180_000,
            instances: [{ browser: "chromium" }],
            screenshotDirectory: ".vitest-screenshots",
          },
          globalSetup: path.resolve(__dirname, "src/runtime/websocket-test-global-setup.ts"),
        },
      },
    ],
    /**
     * Expo pulls in native tooling (xcode, etc.) that executes files relying on `process.send`.
     * Vitest's default worker pool uses worker_threads, which intentionally stub that API and
     * immediately throw `Unexpected call to process.send`. Running the suite in forked processes
     * keeps `process.send` intact so the app tests can boot before hitting the intentional failures.
     */
    pool: "forks",
    maxWorkers: 2,
    server: {
      deps: {
        fallbackCJS: true,
        inline: [
          "react-native-enriched-markdown",
          "zustand",
          "@tanstack/react-query",
          "react-native-web",
          "react-native-reanimated",
          "react-native-worklets",
          "react-native-gesture-handler",
          "react-native-keyboard-controller",
        ],
      },
    },
  },
  // Reanimated and gesture-handler pick platform files by extension
  // (e.g. `GestureHandlerRootView.web.js`). Vite's optimizer does not apply `resolve.extensions`,
  // so it scans the native files and dies on imports react-native-web has no answer for.
  // Unbundled, the same imports go through the resolver below and land on the web files.
  optimizeDeps: {
    // Expo's public navigation barrel also loads screens. Match Metro's web-first resolution
    // while optimizing its CJS dependencies, including DebugContainer.web.js.
    esbuildOptions: {
      resolveExtensions: [
        ".web.js",
        ".web.jsx",
        ".web.ts",
        ".web.tsx",
        ".js",
        ".jsx",
        ".ts",
        ".tsx",
        ".json",
      ],
      plugins: [
        {
          name: "scan-stops-at-host-runtime",
          // The scan for dependencies to bundle follows every static import of every browser test,
          // `vi.mock` or not. The host runtime imports most of the app (the panels, the plugins,
          // every icon), which the stubs below do not cover: one name a stub lacks fails the scan,
          // and then every browser test's dependencies are found one reload at a time. A browser
          // test of something that reads the host runtime mocks it, so the scan stops there too.
          setup(build) {
            build.onResolve({ filter: /^@\/runtime\/host-runtime$/ }, (args) => ({
              path: args.path,
              external: true,
            }));
          },
        },
      ],
    },
    // Bundle the CJS dependencies of the excluded gesture-handler package for the browser.
    include: [
      "react/jsx-runtime",
      "react/compiler-runtime",
      "expo-router/react-navigation",
      "@testing-library/react",
      "@tanstack/react-query",
      "react-i18next",
      "i18next",
      "@marijn/find-cluster-break",
      "tiny-invariant",
      "react/jsx-dev-runtime",
      // What the reanimated-web-utils-esm plugin above imports.
      "react-native-web/dist/exports/StyleSheet/compiler/createReactDOMStyle",
      "react-native-web/dist/exports/StyleSheet/preprocess",
      "react-native-gesture-handler > hoist-non-react-statics",
      "react-native-gesture-handler > invariant",
    ],
    exclude: [
      "react-native-enriched-markdown",
      "react-native-reanimated",
      "react-native-gesture-handler",
      "react-native-keyboard-controller",
    ],
  },
  // The browser tests' server compiles a module when the page asks for it, not ahead of that: a
  // module a test mocks is never asked for, and compiling it ahead (the host runtime, then all it
  // imports) finds dependencies in the middle of a test and reloads the page.
  server: { preTransformRequests: false },
  // babel-preset-expo compiles JSX with the automatic runtime, so app modules don't import React.
  esbuild: { jsx: "automatic" },
  // The globals a React Native bundler defines, which esbuild is no longer there to supply for
  // the package excluded above.
  define: {
    "process.env.JEST_WORKER_ID": "undefined",
    __DEV__: "false",
    global: "globalThis",
  },
  resolve: {
    extensions: [
      ".web.mjs",
      ".web.js",
      ".web.mts",
      ".web.ts",
      ".web.jsx",
      ".web.tsx",
      ".mjs",
      ".js",
      ".mts",
      ".ts",
      ".jsx",
      ".tsx",
      ".json",
    ],
    alias: [
      {
        find: /^@getpaseo\/relay\/e2ee$/,
        replacement: path.resolve(__dirname, "../relay/src/e2ee.ts"),
      },
      {
        find: /^@getpaseo\/relay$/,
        replacement: path.resolve(__dirname, "../relay/src/index.ts"),
      },
      { find: "@", replacement: path.resolve(__dirname, "src") },
      // Keep keyboard-controller's imports in Vite so native aliases and platform extensions apply.
      {
        find: /^react-native-keyboard-controller$/,
        replacement: path.resolve(
          resolvePackageEntry("react-native-keyboard-controller"),
          "lib/module/index.js",
        ),
      },
      // The CJS entry bypasses Vite's React Native alias and web-extension resolution.
      {
        find: /^react-native-gesture-handler$/,
        replacement: path.resolve(
          rootNodeModules,
          "react-native-gesture-handler/lib/module/index.js",
        ),
      },
      // Must precede the `react-native` alias: a string `find` matches by prefix, so this subpath
      // would otherwise resolve inside a react-native-web *file* and break the dependency scan.
      // Reanimated only imports it on the native path, which no test takes.
      {
        find: /^react-native\/Libraries\/Renderer\/shims\/ReactFabric$/,
        replacement: path.resolve(__dirname, "test-stubs/react-native-fabric-shim.ts"),
      },
      // Point to the ESM build so Vite can transform its imports and apply the
      // react alias below (the CJS build uses require('react') which bypasses
      // Vite alias resolution).
      {
        find: "react-native",
        replacement: path.resolve(rootNodeModules, "react-native-web/dist/index.js"),
      },
      { find: "react", replacement: resolvePackageEntry("react") },
      {
        find: "react-dom",
        replacement: resolvePackageEntry("react-dom"),
      },
      {
        find: /^@xterm\/addon-ligatures\/lib\/addon-ligatures\.mjs$/,
        replacement: path.resolve(__dirname, "test-stubs/xterm-addon-ligatures.ts"),
      },
      {
        find: /^@xterm\/addon-ligatures$/,
        replacement: path.resolve(__dirname, "test-stubs/xterm-addon-ligatures.ts"),
      },
      {
        find: /^react-native-unistyles$/,
        replacement: path.resolve(__dirname, "test-stubs/react-native-unistyles.ts"),
      },
      {
        find: /^react-native-svg$/,
        replacement: path.resolve(__dirname, "test-stubs/react-native-svg.ts"),
      },
      // Both ship untranspiled Flow and fail to parse on import, which takes out any test that
      // mounts a menu surface.
      {
        find: /^react-native-safe-area-context$/,
        replacement: path.resolve(__dirname, "test-stubs/react-native-safe-area-context.ts"),
      },
      {
        find: /^@gorhom\/bottom-sheet$/,
        replacement: path.resolve(__dirname, "test-stubs/gorhom-bottom-sheet.ts"),
      },
      {
        find: /^react-native-reanimated\/scripts\/validate-worklets-version$/,
        replacement: path.resolve(__dirname, "test-stubs/reanimated-validate-worklets-version.ts"),
      },
      {
        find: /^expo-clipboard$/,
        replacement: path.resolve(__dirname, "test-stubs/expo-clipboard.ts"),
      },
      {
        find: /^expo-linking$/,
        replacement: path.resolve(__dirname, "test-stubs/expo-linking.ts"),
      },
      // No Node implementation: every call on the real module rejects, so a test
      // touching a persisted store drowns in unhandled rejections.
      {
        find: /^@react-native-async-storage\/async-storage$/,
        replacement: path.resolve(__dirname, "test-stubs/async-storage.ts"),
      },
      {
        find: /^lucide-react-native$/,
        replacement: path.resolve(__dirname, "test-stubs/lucide-react-native.ts"),
      },
    ],
  },
});

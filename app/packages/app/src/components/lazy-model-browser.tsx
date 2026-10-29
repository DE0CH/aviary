import { lazyArea } from "@/components/lazy-area";

// The model browser (the list of providers, models and profiles) is its own chunk, fetched the first time a model
// sheet or selector draws it (docs/paseo-ui.md "Loading"); its state hook and the trigger's glyph are in
// components/model-browser-state.tsx, which the composer needs at once.
export const ModelBrowser = lazyArea(
  "model-browser",
  () => import("@/components/model-browser"),
  (module) => module.ModelBrowser,
  null,
);

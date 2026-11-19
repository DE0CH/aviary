import type { ComponentProps } from "react";
import { lazyArea } from "@/components/lazy-area";
import type * as ModelMenuModule from "@/yyc/model-menu";

// The model and thinking menu is its own chunk, fetched when a composer shows it: its menu, segmented control and
// provider snapshot stay out of the first load (docs/paseo-ui.md "Loading").
const load = () => import("@/yyc/model-menu");

export const YycModelMenu = lazyArea<typeof ModelMenuModule, ComponentProps<typeof ModelMenuModule.YycModelMenu>>(
  "yyc-model-menu",
  load,
  (module) => module.YycModelMenu,
  null,
);

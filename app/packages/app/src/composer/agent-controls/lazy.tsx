import type { ComponentProps } from "react";
import { lazyArea } from "@/components/lazy-area";
import type * as AgentControlsModule from "@/composer/agent-controls";

// The model, mode and thinking controls are their own chunk, fetched the first time a composer shows them; YYC's
// composer never does (composer/input-mode.ts: the model's name instead), so its pages never fetch them
// (docs/paseo-ui.md "Loading").
const load = () => import("@/composer/agent-controls");

export const AgentControls = lazyArea<
  typeof AgentControlsModule,
  ComponentProps<typeof AgentControlsModule.AgentControls>
>("agent-controls", load, (module) => module.AgentControls, null);

export const DraftAgentControls = lazyArea<
  typeof AgentControlsModule,
  ComponentProps<typeof AgentControlsModule.DraftAgentControls>
>("draft-agent-controls", load, (module) => module.DraftAgentControls, null);

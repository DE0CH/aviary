import { lazyArea } from "@/components/lazy-area";

// The settings section is its own chunk: the composer reads profiles through this module, and must not carry the
// settings UI with it (docs/paseo-ui.md "Loading")
export const AgentProfilesSection = lazyArea(
  "agent-profiles-section",
  () => import("./settings/agent-profiles-section"),
  (m) => m.AgentProfilesSection,
  null
);

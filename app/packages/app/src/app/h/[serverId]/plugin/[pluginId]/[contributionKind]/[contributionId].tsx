import { lazyArea } from "@/components/lazy-area";

// A plugin's own screen is its own chunk, fetched when the route opens (docs/paseo-ui.md "Loading")
const PluginSurfaceScreen = lazyArea(
  "plugin-surface",
  () => import("@/plugins/surface-screen"),
  (module) => module.PluginSurfaceScreen,
);

export default function PluginSurfaceRoute() {
  return <PluginSurfaceScreen />;
}

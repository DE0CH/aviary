import { TERMINAL_PROFILE_ICON_NAMES } from "@getpaseo/protocol/provider-icon-names";
import { ACP_PROVIDER_ICON_SVGS } from "@/assets/acp-provider-icons";
import { ACP_PROVIDER_CATALOG } from "@/data/acp-provider-catalog";

// The catalog providers' SVG icons, one chunk fetched the first time one is drawn (components/provider-icons.ts,
// docs/paseo-ui.md "Loading").
export const CATALOG_ICON_SVGS = new Map<string, string>(
  ACP_PROVIDER_CATALOG.flatMap((entry) => (entry.iconSvg ? [[entry.id, entry.iconSvg]] : []))
);

for (const name of TERMINAL_PROFILE_ICON_NAMES) {
  const iconSvg = (ACP_PROVIDER_ICON_SVGS as Record<string, string | undefined>)[name];
  if (iconSvg) {
    CATALOG_ICON_SVGS.set(name, iconSvg);
  }
}

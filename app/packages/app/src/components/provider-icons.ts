import { useStore } from "zustand";
import { providerSnapshotIcons } from "@/data/provider-icons";
import { Bot } from "lucide-react-native";
import { createElement, useCallback, useSyncExternalStore, type ComponentType } from "react";
import { SvgXml } from "react-native-svg";
import { AntigravityIcon } from "@/components/icons/antigravity-icon";
import { ClaudeIcon } from "@/components/icons/claude-icon";
import { CodexIcon } from "@/components/icons/codex-icon";
import { CopilotIcon } from "@/components/icons/copilot-icon";
import { KiroIcon } from "@/components/icons/kiro-icon";
import { MiniMaxIcon } from "@/components/icons/minimax-icon";
import { OpenCodeIcon } from "@/components/icons/opencode-icon";
import { OmpIcon } from "@/components/icons/omp-icon";
import { PiIcon } from "@/components/icons/pi-icon";
import { resolveProviderIconName } from "@/components/provider-icon-name";

export interface ProviderIconProps {
  size: number;
  color: string;
}

export type ProviderIconComponent = ComponentType<ProviderIconProps>;

const BUILTIN_PROVIDER_ICONS: Record<string, ProviderIconComponent> = {
  antigravity: AntigravityIcon as unknown as ProviderIconComponent,
  claude: ClaudeIcon as unknown as ProviderIconComponent,
  codex: CodexIcon as unknown as ProviderIconComponent,
  copilot: CopilotIcon as unknown as ProviderIconComponent,
  kiro: KiroIcon as unknown as ProviderIconComponent,
  minimax: MiniMaxIcon as unknown as ProviderIconComponent,
  omp: OmpIcon as unknown as ProviderIconComponent,
  opencode: OpenCodeIcon as unknown as ProviderIconComponent,
  pi: PiIcon as unknown as ProviderIconComponent,
};

// The catalog providers' icons are their own chunk (components/provider-catalog-icons.ts), fetched the first time one
// is drawn; until then the icon draws nothing (docs/paseo-ui.md "Loading").
let catalogIconSvgs: ReadonlyMap<string, string> | null = null;
let catalogIconsLoad: Promise<void> | null = null;
const catalogIconsListeners = new Set<() => void>();

function loadCatalogIcons(): Promise<void> {
  catalogIconsLoad ??= import("@/components/provider-catalog-icons").then(
    (module) => {
      catalogIconSvgs = module.CATALOG_ICON_SVGS;
      for (const listener of catalogIconsListeners) listener();
    },
    () => {
      catalogIconsLoad = null;
    },
  );
  return catalogIconsLoad;
}

function subscribeCatalogIcons(listener: () => void): () => void {
  catalogIconsListeners.add(listener);
  return () => catalogIconsListeners.delete(listener);
}

function getCatalogIconSvgs(): ReadonlyMap<string, string> | null {
  return catalogIconSvgs;
}

const catalogIconComponents = new Map<string, ProviderIconComponent>();
const snapshotIconComponents = new Map<string, ProviderIconComponent>();

/** Renders a sanitized SVG string from a host catalog, tinted through `currentColor`. */
export function SvgIcon({ svg, size, color }: ProviderIconProps & { svg: string }) {
  return createElement(SvgXml, { xml: svg, width: size, height: size, color });
}

function createSvgIcon(provider: string, iconSvg: string): ProviderIconComponent {
  const SvgProviderIcon: ProviderIconComponent = ({ size, color }) =>
    SvgIcon({ svg: iconSvg, size, color });
  SvgProviderIcon.displayName = `SvgProviderIcon(${provider})`;
  return SvgProviderIcon;
}

function getCatalogProviderIcon(provider: string): ProviderIconComponent {
  const cached = catalogIconComponents.get(provider);
  if (cached) {
    return cached;
  }
  if (catalogIconSvgs) {
    const iconSvg = catalogIconSvgs.get(provider);
    if (!iconSvg) return Bot;
    const icon = createSvgIcon(provider, iconSvg);
    catalogIconComponents.set(provider, icon);
    return icon;
  }
  void loadCatalogIcons();
  const PendingCatalogIcon: ProviderIconComponent = ({ size, color }) => {
    const svgs = useSyncExternalStore(subscribeCatalogIcons, getCatalogIconSvgs, getCatalogIconSvgs);
    if (!svgs) return null;
    const iconSvg = svgs.get(provider);
    return iconSvg ? SvgIcon({ svg: iconSvg, size, color }) : createElement(Bot, { size, color });
  };
  PendingCatalogIcon.displayName = `CatalogProviderIcon(${provider})`;
  catalogIconComponents.set(provider, PendingCatalogIcon);
  return PendingCatalogIcon;
}

function getSnapshotProviderIcon(provider: string, svg: string): ProviderIconComponent {
  const cached = snapshotIconComponents.get(svg);
  if (cached) return cached;
  const component = createSvgIcon(provider, svg);
  snapshotIconComponents.set(svg, component);
  return component;
}

function resolveProviderIcon(provider: string, svg?: string): ProviderIconComponent {
  const name = resolveProviderIconName(provider, svg);
  if (name.kind === "builtin") {
    return BUILTIN_PROVIDER_ICONS[name.id];
  }
  if (name.kind === "catalog") {
    return getCatalogProviderIcon(name.id);
  }
  if (name.kind === "svg") {
    return getSnapshotProviderIcon(provider, name.svg);
  }
  return Bot;
}

export function useProviderIcon(provider: string, serverId?: string | null): ProviderIconComponent {
  const svg = useStore(providerSnapshotIcons, (state) =>
    serverId ? state.get(serverId)?.get(provider) : undefined,
  );
  return resolveProviderIcon(provider, svg);
}

/** Subscribe once for consumers that render a collection of providers. */
export function useProviderIcons(serverId?: string | null) {
  const icons = useStore(providerSnapshotIcons, (state) =>
    serverId ? state.get(serverId) : undefined,
  );
  return useCallback(
    (provider: string) => resolveProviderIcon(provider, icons?.get(provider)),
    [icons],
  );
}

import {
  createElement,
  useEffect,
  useSyncExternalStore,
  type ComponentType,
  type ReactElement,
} from "react";
import type { LucideIcon } from "lucide-react-native";
import {
  findSvgIconProblem,
  type PluginIconProps,
  type PluginSerializableIcon,
  type PluginSvgIcon,
} from "@getpaseo/plugin";
import type { PluginIcon } from "@getpaseo/plugin/client";
import type { IconProps } from "@getpaseo/plugin/client/ui";
import { SvgIcon } from "@/components/provider-icons";

/** Host surfaces may thin a Lucide stroke; SVG and plugin components ignore it. */
type HostIconProps = PluginIconProps & { strokeWidth?: number };
/** A host-drawn icon. A plain function, so it stands in for any plugin icon component type. */
type HostIcon = (props: HostIconProps) => ReactElement | null;

const lucideIconComponents = new Map<string, HostIcon>();
const svgIconComponents = new Map<string, HostIcon>();

// Plugins name any of Lucide's 3,700 icons, so the whole set is its own chunk, fetched the first time a plugin's Lucide
// icon is drawn (docs/paseo-ui.md "Loading"), never when a plugin is merely checked. Until it arrives a name is checked
// only for its shape and draws nothing; a failed fetch is forgotten, so the next draw fetches again.
type LucideModule = typeof import("lucide-react-native");
let lucideIcons: LucideModule | null = null;
let lucideIconsLoad: Promise<LucideModule> | null = null;
const lucideIconsListeners = new Set<() => void>();
const LUCIDE_NAME = /^[A-Z][A-Za-z0-9]*$/;

/** Fetches the Lucide set for plugin icons; resolves at once when it is already here. */
export function loadLucideIcons(): Promise<LucideModule> {
  lucideIconsLoad ??= import("lucide-react-native").then(
    (module) => {
      lucideIcons = module;
      for (const listener of lucideIconsListeners) listener();
      return module;
    },
    (error: unknown) => {
      lucideIconsLoad = null;
      throw error;
    },
  );
  return lucideIconsLoad;
}

function requestLucideIcons(): void {
  loadLucideIcons().catch((error: unknown) => {
    console.warn("[Loading] The Lucide icon set failed to load", error);
  });
}

function subscribeLucideIcons(listener: () => void): () => void {
  lucideIconsListeners.add(listener);
  return () => lucideIconsListeners.delete(listener);
}

function getLucideIcons(): LucideModule | null {
  return lucideIcons;
}

function findLucideIcon(name: string): LucideIcon | null {
  const icons = lucideIcons;
  if (!icons) return null;
  const candidate = Reflect.get(icons, name);
  if (candidate === icons.Icon || candidate === icons.createLucideIcon) return null;
  return isComponent(candidate) ? (candidate as LucideIcon) : null;
}

/**
 * A name shaped like a Lucide icon. Checked only for its shape, whether or not the set has arrived, so a plugin is
 * accepted or refused the same at every load time; an unknown name draws nothing.
 */
function isLucideIconName(name: string): boolean {
  return LUCIDE_NAME.test(name);
}

function isComponent(value: unknown): boolean {
  return (
    typeof value === "function" ||
    (typeof value === "object" && value !== null && "$$typeof" in value)
  );
}

function isSvgIcon(icon: unknown): icon is PluginSvgIcon {
  return (
    typeof icon === "object" &&
    icon !== null &&
    !("$$typeof" in icon) &&
    typeof Reflect.get(icon, "svg") === "string"
  );
}

function isComponentIcon<Props extends PluginIconProps>(
  icon: PluginIcon<Props>,
): icon is ComponentType<Props> {
  return isComponent(icon);
}

function invalidIcon(owner: string, reason: string): Error {
  return new Error(`${owner} has an invalid icon: ${reason}`);
}

/** Validates an icon that is stored with the data it labels. Returns the normalized icon. */
export function parseSerializablePluginIcon(
  owner: string,
  icon: PluginSerializableIcon,
): PluginSerializableIcon {
  if (typeof icon === "string") {
    const name = icon.trim();
    if (!name) throw new Error(`${owner} has no icon`);
    if (!isLucideIconName(name)) throw invalidIcon(owner, `unknown Lucide icon "${name}"`);
    return name;
  }
  if (!isSvgIcon(icon)) throw invalidIcon(owner, "expected a Lucide icon name or { svg }");
  const svg = icon.svg.trim();
  const problem = findSvgIconProblem(svg);
  if (problem) throw invalidIcon(owner, problem);
  return { svg };
}

/** Validates a contribution's icon once, at registration. Rendering trusts the result. */
export function parsePluginIcon<Props extends PluginIconProps>(
  owner: string,
  icon: PluginIcon<Props>,
): PluginIcon<Props> {
  if (isComponentIcon(icon)) return icon;
  if (typeof icon !== "string" && !isSvgIcon(icon)) {
    throw invalidIcon(owner, "expected a Lucide icon name, { svg }, or a component");
  }
  return parseSerializablePluginIcon(owner, icon);
}

function MissingIcon(): null {
  return null;
}

/** Stands in for a Lucide icon until the set arrives (it asks for it once drawn), then draws it. */
function PendingLucideIcon({ name, ...props }: HostIconProps & { name: string }) {
  const icons = useSyncExternalStore(subscribeLucideIcons, getLucideIcons, getLucideIcons);
  useEffect(() => {
    if (!icons) requestLucideIcons();
  }, [icons]);
  const Lucide = findLucideIcon(name);
  return Lucide ? createElement(Lucide, props) : null;
}

function createLucideIcon(name: string): HostIcon {
  function PluginLucideIcon({ size, color, strokeWidth }: HostIconProps) {
    if (!lucideIcons) return createElement(PendingLucideIcon, { name, size, color, strokeWidth });
    const Lucide = findLucideIcon(name);
    return Lucide ? createElement(Lucide, { size, color, strokeWidth }) : null;
  }
  PluginLucideIcon.displayName = `PluginLucideIcon(${name})`;
  return PluginLucideIcon;
}

function lucideIconComponent(name: string): HostIcon {
  const cached = lucideIconComponents.get(name);
  if (cached) return cached;
  if (!isLucideIconName(name)) return MissingIcon;
  const icon = createLucideIcon(name);
  lucideIconComponents.set(name, icon);
  return icon;
}

function svgIconComponent(svg: string): HostIcon {
  const cached = svgIconComponents.get(svg);
  if (cached) return cached;
  function PluginSvgIcon({ size, color }: PluginIconProps) {
    return createElement(SvgIcon, { svg, size, color });
  }
  svgIconComponents.set(svg, PluginSvgIcon);
  return PluginSvgIcon;
}

/**
 * A stable component for a registered icon, for surfaces that take an icon component. Lucide and
 * SVG icons draw from `size` and `color` only; a plugin component receives every prop.
 */
export function pluginIconComponent<Props extends PluginIconProps>(
  icon: PluginIcon<Props>,
): ComponentType<Props> {
  if (typeof icon === "string") return lucideIconComponent(icon);
  if (isSvgIcon(icon)) return svgIconComponent(icon.svg);
  return icon;
}

export interface PluginIconViewProps extends PluginIconProps {
  icon: PluginIcon;
}

/** Draws a registered plugin icon. `color` tints Lucide icons and `currentColor` in SVG. */
export function PluginIconView({ icon, size, color }: PluginIconViewProps): ReactElement {
  return createElement(pluginIconComponent(icon), { size, color });
}

function isRenderable(icon: PluginIcon): boolean {
  // Drawing (unlike validation) may be exact once the set is here: an unknown name renders nothing either way
  if (typeof icon === "string") return isLucideIconName(icon) && (!lucideIcons || findLucideIcon(icon) !== null);
  if (isSvgIcon(icon)) return findSvgIconProblem(icon.svg) === null;
  return isComponent(icon);
}

/** The kit `Icon`. Plugin code passes it anything, so an unusable icon renders nothing. */
export function Icon(props: IconProps): ReactElement | null {
  const icon = props.icon === undefined ? props.name : props.icon;
  if (!isRenderable(icon)) return null;
  const size = props.size ?? 24;
  const color = props.color ?? "currentColor";
  return createElement(PluginIconView, { icon, size, color });
}

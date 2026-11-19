import { useSyncExternalStore, type ComponentType } from "react";
import { View } from "react-native";
import { SvgXml } from "react-native-svg";
import type { PanelIconProps } from "@/panels/panel-registry";

// The file-type icon table (material-icon-theme, ~13 KB brotli) is its own chunk, fetched the first time a file icon
// is drawn; until then the icon's square stays empty (docs/paseo-ui.md "Loading").
type FileIconModule = typeof import("@/components/file-icon-svg");
let fileIcons: FileIconModule | null = null;
let fileIconsLoad: Promise<void> | null = null;
const listeners = new Set<() => void>();

function loadFileIcons(): void {
  fileIconsLoad ??= import("@/components/file-icon-svg").then(
    (module) => {
      fileIcons = module;
      for (const listener of listeners) listener();
    },
    () => {
      fileIconsLoad = null;
    }
  );
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  loadFileIcons();
  return () => listeners.delete(listener);
}

function getFileIcons(): FileIconModule | null {
  return fileIcons;
}

export function MaterialFileIcon({ fileName, size }: { fileName: string; size: number }) {
  const icons = useSyncExternalStore(subscribe, getFileIcons, getFileIcons);
  if (!icons) return <View style={{ width: size, height: size }} />;
  return <SvgXml xml={icons.getFileIconSvg(fileName)} width={size} height={size} />;
}

export function createMaterialFileIcon(fileName: string): ComponentType<PanelIconProps> {
  function BoundMaterialFileIcon({ size }: PanelIconProps) {
    return <MaterialFileIcon fileName={fileName} size={size} />;
  }
  return BoundMaterialFileIcon;
}

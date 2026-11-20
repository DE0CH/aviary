import { revealWorkspaceTab } from "@/workspace-tabs/navigation";
import { usePanelStore } from "@/stores/panel-store";
import AsyncStorage from "@react-native-async-storage/async-storage";
import { useLocalSearchParams, usePathname } from "expo-router";
import { useEffect, useSyncExternalStore } from "react";
import { isNative } from "@/constants/platform";
import {
  createLastWorkspaceSelectionStore,
  LAST_WORKSPACE_SELECTION_STORAGE_KEY,
  type ActiveWorkspaceSelection,
  type LastWorkspaceSelectionStorage,
} from "@/stores/last-workspace-selection";
import {
  navigateToLastWorkspace as navigateToLastWorkspacePure,
  navigateToWorkspace as navigateToWorkspacePure,
  parseActiveWorkspaceSelection,
  type NavigateToWorkspaceDeps,
  type NavigateToWorkspaceInput,
} from "./navigation";
import { shownSelection, type TappedSelection } from "./tapped-selection";
import { useSessionStore } from "@/stores/session-store";
import { useWorkspaceLayoutStore } from "@/stores/workspace-layout-store";
import { stripHostWorkspaceRouteEchoSearchFromBrowserUrlAfterCommit } from "@/utils/host-route-browser";
import { navigateToHostWorkspaceRoute } from "@/navigation/workspace-route-navigation";
import { useViewAsStore } from "@/yyc/view-as";

export type { ActiveWorkspaceSelection } from "@/stores/last-workspace-selection";
export type { NavigateToWorkspaceInput } from "./navigation";

// A view-as tab remembers the person's last conversation under its own key, never the admin's (yyc/view-as.ts).
const viewAsUser = useViewAsStore.getState().viewAs?.user;
const lastWorkspaceSelectionKey = viewAsUser
  ? `${LAST_WORKSPACE_SELECTION_STORAGE_KEY}.view-as.${viewAsUser}`
  : LAST_WORKSPACE_SELECTION_STORAGE_KEY;

const lastWorkspaceSelectionStorage: LastWorkspaceSelectionStorage = {
  read: () => AsyncStorage.getItem(lastWorkspaceSelectionKey),
  write: (value) => AsyncStorage.setItem(lastWorkspaceSelectionKey, value),
  clear: () => AsyncStorage.removeItem(lastWorkspaceSelectionKey),
};

const lastWorkspaceSelectionStore = createLastWorkspaceSelectionStore(
  lastWorkspaceSelectionStorage,
);

function navigateDeps(): NavigateToWorkspaceDeps {
  return {
    revealTab: revealWorkspaceTab,
    revealWorkspace: () => {
      if (useWorkspaceLayoutStore.getState().layoutMode === "compact") {
        usePanelStore.getState().showMobileAgent();
      }
    },
    getSessionWorkspaces: (serverId) => useSessionStore.getState().sessions[serverId]?.workspaces,
    getSessionAgents: (serverId) =>
      useSessionStore.getState().sessions[serverId]?.agents.values() ?? [],
    isWorkspaceLayoutHydrated: () => useWorkspaceLayoutStore.persist.hasHydrated(),
    deferOpenTab: (input) => useWorkspaceLayoutStore.getState().deferOpenTab(input),
    openTab: (input) => useWorkspaceLayoutStore.getState().openTab(input),
    rememberLastWorkspace: (selection) => lastWorkspaceSelectionStore.remember(selection),
    navigateToRoute: (route) => {
      navigateToHostWorkspaceRoute(route);
      stripHostWorkspaceRouteEchoSearchFromBrowserUrlAfterCommit();
    },
  };
}

export function hydrateLastWorkspaceSelection(): Promise<void> {
  return lastWorkspaceSelectionStore.hydrate();
}

export function getLastWorkspaceSelection(): ActiveWorkspaceSelection | null {
  return lastWorkspaceSelectionStore.getSelection();
}

export function getIsLastWorkspaceSelectionHydrated(): boolean {
  return lastWorkspaceSelectionStore.isHydrated();
}

export function navigateToWorkspace(input: NavigateToWorkspaceInput): string {
  return navigateToWorkspacePure(input, navigateDeps());
}

export function navigateToLastWorkspace(): boolean {
  return navigateToLastWorkspacePure({
    ...navigateDeps(),
    getLastWorkspaceSelection: () => lastWorkspaceSelectionStore.getSelection(),
  });
}

export function useActiveWorkspaceSelection(): ActiveWorkspaceSelection | null {
  const params = useLocalSearchParams<{
    serverId?: string | string[];
    workspaceId?: string | string[];
  }>();
  const selection = parseActiveWorkspaceSelection({ pathname: usePathname(), params });
  const serverId = selection?.serverId ?? null;
  const workspaceId = selection?.workspaceId ?? null;
  useEffect(() => {
    if (!serverId || !workspaceId) {
      return;
    }
    lastWorkspaceSelectionStore.remember({ serverId, workspaceId });
  }, [serverId, workspaceId]);
  return selection;
}

export function useLastWorkspaceSelection(): ActiveWorkspaceSelection | null {
  return useSyncExternalStore(
    lastWorkspaceSelectionStore.subscribe,
    getLastWorkspaceSelection,
    getLastWorkspaceSelection,
  );
}

export function useIsLastWorkspaceSelectionHydrated(): boolean {
  return useSyncExternalStore(
    lastWorkspaceSelectionStore.subscribe,
    getIsLastWorkspaceSelectionHydrated,
    getIsLastWorkspaceSelectionHydrated,
  );
}

void hydrateLastWorkspaceSelection();

// A conversation tapped in the list is selected at once (tapped-selection.ts). On the phone apps the conversation
// opens a frame later, so the row is drawn selected before the new conversation's render holds the JS thread; the
// web opens it in the same task, as before.
let tapped: TappedSelection | null = null;
let routeSelection: ActiveWorkspaceSelection | null = null;
const tappedListeners = new Set<() => void>();
function subscribeTapped(listener: () => void): () => void {
  tappedListeners.add(listener);
  return () => tappedListeners.delete(listener);
}
const getTapped = (): TappedSelection | null => tapped;

/** The list's selection: the route's, or the conversation just tapped while the route has not moved yet. */
export function useShownWorkspaceSelection(): ActiveWorkspaceSelection | null {
  const route = useActiveWorkspaceSelection();
  const current = useSyncExternalStore(subscribeTapped, getTapped, getTapped);
  useEffect(() => {
    routeSelection = route;
  }, [route]);
  return shownSelection(route, current);
}

/** Opens a conversation tapped in the list. */
export function openWorkspaceFromList(input: NavigateToWorkspaceInput): void {
  if (!isNative) {
    navigateToWorkspace(input);
    return;
  }
  tapped = { from: routeSelection, to: { serverId: input.serverId, workspaceId: input.workspaceId } };
  tappedListeners.forEach((listener) => listener());
  requestAnimationFrame(() => navigateToWorkspace(input));
}

import type { ActiveWorkspaceSelection } from "@/stores/last-workspace-selection";

/**
 * A conversation tapped in the list, shown selected before the route reaches it (Deyao, 2026-11-20: on the Android
 * app the tapped row lit up only once the conversation had opened). `from` is the route when it was tapped.
 */
export interface TappedSelection {
  from: ActiveWorkspaceSelection | null;
  to: ActiveWorkspaceSelection;
}

function sameSelection(a: ActiveWorkspaceSelection | null, b: ActiveWorkspaceSelection | null): boolean {
  return a?.serverId === b?.serverId && a?.workspaceId === b?.workspaceId;
}

/** The tapped row stands until the route moves, wherever it moves to; then the route is the selection again. */
export function shownSelection(
  route: ActiveWorkspaceSelection | null,
  tapped: TappedSelection | null,
): ActiveWorkspaceSelection | null {
  return tapped && sameSelection(route, tapped.from) ? tapped.to : route;
}

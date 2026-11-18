import type { SidebarStateBucket } from "@/utils/sidebar-agent-state";

export type ProjectStatusBadgeDotBucket = "failed" | "attention" | "running" | "needs_input";

export type ProjectStatusBadgeContent = { kind: "dot"; bucket: ProjectStatusBadgeDotBucket };

/**
 * What the project status badge should render for a project's aggregate bucket, or null when
 * no badge should show at all. Kept as plain data (no React) so it's testable without JSDOM
 * or component mounting — see docs/testing.md's two test categories.
 *
 * Every surfaced bucket is a dot of its own colour; needs_input is the amber one (Deyao,
 * 2026-11-18: no glyph). The badge is 12pt; anything with internal detail at that size loses
 * to a solid disc, so the buckets separate by colour and — for running alone — by pulsing.
 */
export function getProjectStatusBadgeContent(
  statusBucket: SidebarStateBucket | null,
): ProjectStatusBadgeContent | null {
  if (
    statusBucket === "needs_input" ||
    statusBucket === "failed" ||
    statusBucket === "attention" ||
    statusBucket === "running"
  ) {
    return { kind: "dot", bucket: statusBucket };
  }
  return null;
}

/**
 * @vitest-environment jsdom
 */
import React, { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const layout = vi.hoisted(() => ({ compact: true }));
const badges = vi.hoisted(() => ({ last: null as null | Record<string, unknown> }));

vi.mock("react-native", () => ({
  ScrollView: ({ children }: React.PropsWithChildren) =>
    React.createElement("div", { "data-testid": "own-scroll" }, children),
  View: ({ children }: React.PropsWithChildren) => React.createElement("div", null, children),
}));
vi.mock("react-native-unistyles", () => ({
  StyleSheet: { create: () => ({}) },
}));
vi.mock("lucide-react-native", () => ({ Wrench: () => null }));
vi.mock("@/components/indent-rail", () => ({
  IndentRail: ({ children }: React.PropsWithChildren) => React.createElement("div", null, children),
}));
vi.mock("@/constants/layout", () => ({ useIsCompactFormFactor: () => layout.compact }));
vi.mock("./label", () => ({ useOverviewSummaryLabel: () => "Read 1 file" }));
vi.mock("@/components/message", () => ({
  ExpandableBadge: (props: Record<string, unknown> & { renderDetails?: () => React.ReactNode }) => {
    badges.last = props;
    return React.createElement(
      "div",
      { "data-testid": props.testID as string },
      props.isExpanded && props.renderDetails ? props.renderDetails() : null,
    );
  },
}));

vi.stubGlobal("React", React);
vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);

import { RowAnchorContext, type RowAnchor } from "@/agent-stream/row-anchor";
import { OverviewToolCallGroupView } from "./view";
import type { OverviewToolCallGroup } from "./model";

const group = {
  mode: "overview",
  run: { id: "run-1" },
  summary: { editedFileCount: 0, commandCount: 0, readFileCount: 1, searchCount: 0, otherToolCount: 0, paseoCallCount: 0 },
  isLoading: false,
} as unknown as OverviewToolCallGroup;

describe("OverviewToolCallGroupView", () => {
  let container: HTMLElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    badges.last = null;
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  function render(expanded: boolean, anchor: RowAnchor | null = null) {
    act(() => {
      root.render(
        <RowAnchorContext.Provider value={anchor}>
          <OverviewToolCallGroupView group={group} expanded={expanded} isLastInSequence onExpandedChange={() => {}}>
            <span data-testid="the-calls">calls</span>
          </OverviewToolCallGroupView>
        </RowAnchorContext.Provider>,
      );
    });
  }

  // Deyao, 2026-11-20: a tool call opens in place on a phone too, never in a bottom sheet; a group of them is one
  it.each([true, false])("opens its calls in place below its header (compact %s)", (compact) => {
    layout.compact = compact;
    render(true);
    expect(badges.last?.isExpanded).toBe(true);
    const badge = container.querySelector('[data-testid="tool-call-group"]');
    expect(badge?.querySelector('[data-testid="the-calls"]')).not.toBeNull();
    expect(container.querySelectorAll('[data-testid="the-calls"]')).toHaveLength(1);
  });

  it("shows only its header while closed", () => {
    layout.compact = true;
    render(false);
    expect(badges.last?.isExpanded).toBe(false);
    expect(container.querySelector('[data-testid="the-calls"]')).toBeNull();
  });

  // A list that holds a row's header still (the phone's, agent-stream/row-anchor.tsx) scrolls by each frame's growth
  // of the row; a box of its own capped at 400 px would grow less than its calls do, and the header would slide down
  // by the difference (the iPhone walk: a call's header moved 105 px as it opened). Without that list, the box stays.
  it("lets the list do the scrolling where the list holds the header still", () => {
    render(true, { keepTop: () => {} });
    expect(container.querySelector('[data-testid="own-scroll"]')).toBeNull();
    expect(container.querySelector('[data-testid="the-calls"]')).not.toBeNull();
  });

  it("keeps its own capped scroll box elsewhere", () => {
    render(true, null);
    expect(container.querySelector('[data-testid="own-scroll"] [data-testid="the-calls"]')).not.toBeNull();
  });
});

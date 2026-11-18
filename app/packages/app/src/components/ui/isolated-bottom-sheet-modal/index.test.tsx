/**
 * @vitest-environment jsdom
 */
import React, { act, createContext, useContext, useSyncExternalStore } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

/**
 * A faithful stand-in for `@gorhom/portal`: it renders *nothing* where it sits and hands its
 * element to a host somewhere else in the tree, which is what makes the real thing lose context.
 * A passthrough fake would keep context and quietly pass every test in this file — which is
 * exactly what happened, and why the menu sheet shipped broken.
 */
const portalStore = vi.hoisted(() => {
  const listeners = new Set<() => void>();
  let node: React.ReactNode = null;
  return {
    put(next: React.ReactNode) {
      node = next;
      for (const listener of listeners) listener();
    },
    read: () => node,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    reset() {
      node = null;
      listeners.clear();
    },
  };
});

vi.mock("@gorhom/portal", () => ({
  Portal: ({ children }: { children?: React.ReactNode }) => {
    React.useEffect(() => {
      portalStore.put(children);
    }, [children]);
    return null;
  },
  PortalHost: () => React.createElement("div", { "data-host": true }),
}));

/** What the wrapper hands Gorhom, for the layout tests below to feed into Gorhom's own geometry. */
const sheetProps = vi.hoisted(() => ({ topInset: undefined as number | undefined }));

/** An iPhone with a Dynamic Island: 852 pt tall, the status bar 59 pt. */
const IPHONE = vi.hoisted(() => ({ height: 852, insets: { top: 59, right: 0, bottom: 34, left: 0 } }));
vi.mock("react-native-safe-area-context", () => ({ useSafeAreaInsets: () => IPHONE.insets }));

vi.mock("@gorhom/bottom-sheet", async () => {
  const { Portal } = await import("@gorhom/portal");
  return {
    BottomSheetModalProvider: ({ children }: { children?: React.ReactNode }) =>
      React.createElement("div", { "data-bottom-sheet-provider": true }, children),
    BottomSheetModal: React.forwardRef(
      (
        {
          children,
          stackBehavior,
          topInset,
        }: { children?: React.ReactNode; stackBehavior?: string; topInset?: number },
        _ref,
      ) => {
        sheetProps.topInset = topInset;
        return React.createElement(
          Portal,
          null,
          React.createElement("div", { "data-stack-behavior": stackBehavior }, children),
        );
      },
    ),
  };
});

import { IsolatedBottomSheetModal, type ContextBridge } from ".";
import { useAnimatedLayout } from "@gorhom/bottom-sheet/src/hooks/useAnimatedLayout";
import { normalizeSnapPoint } from "@gorhom/bottom-sheet/src/utilities/normalizeSnapPoint";
import { makeMutable } from "react-native-reanimated";
import { useIsInsideBottomSheet } from "@/components/ui/bottom-sheet-scope";

/** Renders whatever the portal is currently holding, from its own place in the tree. */
function PortalHostProbe() {
  const node = useSyncExternalStore(portalStore.subscribe, portalStore.read, portalStore.read);
  return <div data-portal-host>{node}</div>;
}

const LabelContext = createContext("unbridged");

function Label() {
  return <div data-label={useContext(LabelContext)} />;
}

function BottomSheetScopeProbe() {
  return <div data-inside-bottom-sheet={useIsInsideBottomSheet().toString()} />;
}

describe("IsolatedBottomSheetModal presentation", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    portalStore.reset();
  });

  function stackBehaviors(): (string | null)[] {
    return Array.from(container.querySelectorAll("[data-stack-behavior]")).map((node) =>
      node.getAttribute("data-stack-behavior"),
    );
  }

  it("defaults sibling top-level sheets to push instead of replacing by React ancestry", () => {
    act(() => {
      root.render(
        <>
          <IsolatedBottomSheetModal contextBridge={null}>Settings</IsolatedBottomSheetModal>
          <PortalHostProbe />
        </>,
      );
    });
    expect(stackBehaviors()).toEqual(["push"]);

    act(() => {
      root.render(
        <>
          <IsolatedBottomSheetModal contextBridge={null}>Diagnostic</IsolatedBottomSheetModal>
          <PortalHostProbe />
        </>,
      );
    });
    expect(stackBehaviors()).toEqual(["push"]);
  });

  it("only replaces when the callsite asks for replacement", () => {
    act(() => {
      root.render(
        <>
          <IsolatedBottomSheetModal presentation="replace" contextBridge={null}>
            Selector
          </IsolatedBottomSheetModal>
          <PortalHostProbe />
        </>,
      );
    });

    expect(stackBehaviors()).toEqual(["replace"]);
  });
});

describe("IsolatedBottomSheetModal context bridging", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    portalStore.reset();
  });

  function renderSheet(contextBridge: ContextBridge | null) {
    act(() => {
      root.render(
        <>
          <LabelContext.Provider value="from callsite">
            <IsolatedBottomSheetModal contextBridge={contextBridge}>
              <Label />
            </IsolatedBottomSheetModal>
          </LabelContext.Provider>
          {/* Outside the provider, standing in for the app-root portal host. */}
          <PortalHostProbe />
        </>,
      );
    });
    return container.querySelector("[data-label]")?.getAttribute("data-label");
  }

  it("loses callsite context without a bridge", () => {
    expect(renderSheet(null)).toBe("unbridged");
  });

  it("carries callsite context across the portal when bridged", () => {
    const bridge: ContextBridge = (children) => (
      <LabelContext.Provider value="from callsite">{children}</LabelContext.Provider>
    );
    expect(renderSheet(bridge)).toBe("from callsite");
  });

  it("marks content rendered through the modal as inside a bottom sheet", () => {
    act(() => {
      root.render(
        <>
          <BottomSheetScopeProbe />
          <IsolatedBottomSheetModal contextBridge={null}>
            <BottomSheetScopeProbe />
          </IsolatedBottomSheetModal>
          <PortalHostProbe />
        </>,
      );
    });

    expect(
      Array.from(container.querySelectorAll("[data-inside-bottom-sheet]")).map((node) =>
        node.getAttribute("data-inside-bottom-sheet"),
      ),
    ).toEqual(["false", "true"]);
  });
});

describe("bottom sheet layout subscription", () => {
  it("stops updating a dismissed sheet when the window resizes", () => {
    const container = document.createElement("div");
    const root = createRoot(container);
    let layout: ReturnType<typeof useAnimatedLayout> | undefined;
    const viewport = document.documentElement;
    const originalWidth = Object.getOwnPropertyDescriptor(viewport, "clientWidth");
    function resize(width: number) {
      Object.defineProperty(viewport, "clientWidth", { configurable: true, value: width });
      act(() => window.dispatchEvent(new Event("resize")));
    }
    function LayoutProbe() {
      layout = useAnimatedLayout(undefined, 0, 0);
      return null;
    }
    try {
      act(() => root.render(<LayoutProbe />));
      if (!layout) throw new Error("Sheet layout did not mount");
      const mountedLayout = layout;
      resize(640);
      expect(mountedLayout.get().window.width).toBe(640);
      act(() => root.render(null));
      resize(960);
      expect(mountedLayout.get().window.width).toBe(640);
    } finally {
      act(() => root.unmount());
      if (originalWidth) Object.defineProperty(viewport, "clientWidth", originalWidth);
      else Reflect.deleteProperty(viewport, "clientWidth");
    }
  });
});

// Deyao, 2026-11-18: the Thinking sheet, opened tall on an iPhone, ran up under the status bar. Every sheet goes
// through this wrapper, so the wrapper is where a sheet learns where the safe area starts.
describe("IsolatedBottomSheetModal and the safe area", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    sheetProps.topInset = undefined;
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    portalStore.reset();
  });

  /** Where Gorhom puts the top of a sheet at `snapPoint`, in a window-tall host, with the inset the wrapper gave it. */
  function sheetTop(snapPoint: string): number {
    const topInset = sheetProps.topInset ?? 0;
    let containerHeight = -1;
    function LayoutProbe() {
      const containerLayoutState = React.useMemo(
        () =>
          makeMutable({ height: IPHONE.height, offset: { top: 0, bottom: 0, left: 0, right: 0 } }),
        [],
      );
      containerHeight = useAnimatedLayout(containerLayoutState, topInset, 0, true).get()
        .containerHeight;
      return null;
    }
    const probeRoot = createRoot(document.createElement("div"));
    act(() => probeRoot.render(<LayoutProbe />));
    act(() => probeRoot.unmount());
    // Gorhom places a detent from the top of its room and offsets the room by the inset.
    return topInset + normalizeSnapPoint(snapPoint, containerHeight);
  }

  it("keeps a tall sheet below the status bar without the sheet asking", () => {
    act(() => {
      root.render(
        <IsolatedBottomSheetModal contextBridge={null} snapPoints={["60%", "95%"]}>
          Thinking
        </IsolatedBottomSheetModal>,
      );
    });
    expect(sheetTop("95%")).toBeGreaterThanOrEqual(IPHONE.insets.top);
    expect(sheetTop("100%")).toBe(IPHONE.insets.top);
  });

  it("lets a sheet that knows better set its own top inset", () => {
    act(() => {
      root.render(
        <IsolatedBottomSheetModal contextBridge={null} topInset={0}>
          Full height
        </IsolatedBottomSheetModal>,
      );
    });
    expect(sheetTop("100%")).toBe(0);
  });
});

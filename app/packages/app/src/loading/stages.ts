import { useSyncExternalStore } from "react";
import { loadWSOutboundValidator } from "@getpaseo/client/internal/daemon-client";
import { isWeb } from "@/constants/platform";

// The order the web app's pieces arrive in (Deyao, 2026-10-29; docs/paseo-ui.md "Loading"): the message box and the
// connection come with the entry chunk; then the reply's display (timeline, markdown, code highlighting); then the
// conversation list; everything else only when used. Each stage's import() starts when the one before it has
// arrived, so a later chunk never takes bandwidth from an earlier one:
// - stage 1, the reply chunk: once the server-message validator (the connection's own chunk) has arrived or failed;
// - stage 2, the sidebar chunk: once stage 1 has settled;
// - stage 3, the hosts chunk (dialogs, sheets, callouts; loading/hosts-chunk.tsx): at idle after stage 2.
// Being rendered never starts a chunk early (a lazy area waits for its stage); a person's own action does: a send
// fetches the reply chunk at once, opening the phone drawer the sidebar. Native has every module in one bundle, so
// every stage is reached from the start.
export type LoadingStage = 0 | 1 | 2 | 3;

let reached: LoadingStage = isWeb ? 0 : 3;
const listeners = new Set<() => void>();
const stageWaiters = new Map<LoadingStage, Array<() => void>>();

function reach(stage: LoadingStage): void {
  if (stage <= reached) return;
  reached = stage;
  for (const [waiting, resolvers] of stageWaiters) {
    if (waiting > stage) continue;
    stageWaiters.delete(waiting);
    for (const resolve of resolvers) resolve();
  }
  for (const listener of listeners) listener();
}

/** Resolves once `stage` has been reached. */
export function waitForStage(stage: LoadingStage): Promise<void> {
  if (stage <= reached) return Promise.resolve();
  return new Promise((resolve) => {
    const resolvers = stageWaiters.get(stage) ?? [];
    resolvers.push(resolve);
    stageWaiters.set(stage, resolvers);
  });
}

export function hasReachedStage(stage: LoadingStage): boolean {
  return stage <= reached;
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** Re-renders when the app reaches `stage`. */
export function useStageReached(stage: LoadingStage): boolean {
  return useSyncExternalStore(
    subscribe,
    () => stage <= reached,
    () => stage <= reached,
  );
}

function markChunk(name: string): void {
  try {
    performance.mark(`yyc:chunk-requested:${name}`);
  } catch {
    // performance marks are for load-timing only
  }
}

function settle(promise: Promise<unknown>): Promise<void> {
  return promise.then(
    () => undefined,
    () => undefined,
  );
}

let replyChunk: Promise<typeof import("./reply-chunk")> | null = null;
let sidebarChunk: Promise<typeof import("./sidebar-chunk")> | null = null;

/** The reply's display: the timeline and the markdown renderer. Starts the fetch now (a send, a stage). */
export function loadReplyChunk(): Promise<typeof import("./reply-chunk")> {
  if (!replyChunk) {
    markChunk("reply");
    // A failed fetch is forgotten, so a retry fetches again
    replyChunk = import("./reply-chunk").catch((error: unknown) => {
      replyChunk = null;
      throw error;
    });
  }
  return replyChunk;
}

/** The reply chunk once its stage has come, for a lazy area that is merely rendered. */
export function loadReplyChunkInTurn(): Promise<typeof import("./reply-chunk")> {
  return waitForStage(1).then(loadReplyChunk);
}

/** The conversation list. Starts the fetch now; only the phone drawer, opened by hand, calls this before stage 2. */
export function loadSidebarChunk(): Promise<typeof import("./sidebar-chunk")> {
  if (!sidebarChunk) {
    markChunk("sidebar");
    sidebarChunk = import("./sidebar-chunk").catch((error: unknown) => {
      sidebarChunk = null;
      throw error;
    });
  }
  return sidebarChunk;
}

let requestSidebarNow: (() => void) | null = null;
const sidebarRequested = new Promise<void>((resolve) => {
  requestSidebarNow = resolve;
});

/** A person opened the phone drawer: the sidebar chunk jumps the queue. */
export function requestSidebarChunkNow(): void {
  requestSidebarNow?.();
  void loadSidebarChunk();
}

/** The sidebar chunk once its stage has come (never before the reply chunk has settled), or once asked for. */
export function loadSidebarChunkInTurn(): Promise<typeof import("./sidebar-chunk")> {
  return Promise.race([waitForStage(2), sidebarRequested]).then(loadSidebarChunk);
}

/** The hosts chunk (loading/hosts-chunk.tsx), once stage 3 has come. */
export function loadHostsChunkInTurn(): Promise<typeof import("./hosts-chunk")> {
  return waitForStage(3).then(() => {
    markChunk("hosts");
    return import("./hosts-chunk");
  });
}

function onIdle(run: () => void): void {
  const requestIdle = (globalThis as { requestIdleCallback?: (cb: () => void, o?: object) => void })
    .requestIdleCallback;
  if (requestIdle) requestIdle(run, { timeout: 2_000 });
  else setTimeout(run, 200);
}

let started = false;

/** Runs the stages on web; called once when the entry chunk evaluates. */
export function startLoadingStages(): void {
  if (started || !isWeb) return;
  started = true;
  void settle(loadWSOutboundValidator())
    .then(() => {
      reach(1);
      return settle(loadReplyChunk());
    })
    .finally(() => {
      reach(2);
      void settle(loadSidebarChunk()).finally(() => onIdle(() => reach(3)));
    });
}

export const __private__ = {
  reset(stage: LoadingStage = 0): void {
    reached = stage;
    started = false;
    replyChunk = null;
    sidebarChunk = null;
    stageWaiters.clear();
  },
  reach,
};

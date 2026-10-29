import { Suspense, type ComponentType, type Ref } from "react";
import { ChunkErrorBoundary, LoadingFrame } from "@/components/loading-frame";
import { loadReplyChunkInTurn } from "@/loading/stages";
import { lazyWithRetry } from "@/utils/lazy-with-retry";
import type { AgentStreamViewHandle, AgentStreamViewProps } from "./view";

export type { AgentStreamViewHandle, AgentStreamViewProps } from "./view";

// The timeline is in the reply chunk, fetched in its turn after the connection (loading/stages.ts). Its Suspense sits
// right around it, so the composer beside it is there from the first render.
type LazyAgentStreamViewProps = AgentStreamViewProps & { ref?: Ref<AgentStreamViewHandle> };

const LazyAgentStreamView = lazyWithRetry(
  loadReplyChunkInTurn,
  (chunk) => chunk.AgentStreamView as ComponentType<LazyAgentStreamViewProps>,
);

export function AgentStreamView(props: LazyAgentStreamViewProps) {
  return (
    <ChunkErrorBoundary name="reply">
      <Suspense fallback={<LoadingFrame />}>
        <LazyAgentStreamView {...props} />
      </Suspense>
    </ChunkErrorBoundary>
  );
}

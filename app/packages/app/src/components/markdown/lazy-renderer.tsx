import { Suspense } from "react";
import { ChunkErrorBoundary } from "@/components/loading-frame";
import { loadReplyChunkInTurn } from "@/loading/stages";
import { lazyWithRetry } from "@/utils/lazy-with-retry";
import type { MarkdownRendererProps } from "./renderer";

// Markdown outside the timeline (the changelog, plugin views, file previews, pull requests) shares the reply chunk
// with it (loading/stages.ts, docs/paseo-ui.md "Loading").
const LazyMarkdownRenderer = lazyWithRetry(loadReplyChunkInTurn, (chunk) => chunk.MarkdownRenderer);

export function MarkdownRenderer(props: MarkdownRendererProps) {
  return (
    <ChunkErrorBoundary name="reply">
      <Suspense fallback={null}>
        <LazyMarkdownRenderer {...props} />
      </Suspense>
    </ChunkErrorBoundary>
  );
}

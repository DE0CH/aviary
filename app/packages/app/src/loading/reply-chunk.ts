// The reply's display, one chunk: the timeline with its message rendering, markdown and code highlighting
// (loading/stages.ts, docs/paseo-ui.md "Loading").
export { AgentStreamView } from "@/agent-stream/view";
export { MarkdownRenderer } from "@/components/markdown/renderer";

try {
  performance.mark("yyc:chunk:reply");
} catch {
  // performance marks are for load-timing only
}

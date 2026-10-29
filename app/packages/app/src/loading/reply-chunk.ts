// The reply's display, one chunk: the timeline with its message rendering and markdown; code highlighting is its own
// chunk (utils/highlight-grammars.ts) that stage 1 fetches alongside (loading/stages.ts, docs/paseo-ui.md "Loading").
export { AgentStreamView } from "@/agent-stream/view";
export { MarkdownRenderer } from "@/components/markdown/renderer";

try {
  performance.mark("yyc:chunk:reply");
} catch {
  // performance marks are for load-timing only
}

// The reply's display, one chunk: the timeline with its message rendering and markdown; code highlighting is its own
// chunk (utils/highlight-grammars.ts) that stage 1 fetches alongside (loading/stages.ts, docs/paseo-ui.md "Loading").
export { AgentStreamView } from "@/agent-stream/view";
export { MarkdownRenderer } from "@/components/markdown/renderer";
// the line under a message the server has not confirmed: the queue above the message box (entry chunk) draws it from here
export { PendingSendLine } from "@/yyc/pending-send";

try {
  performance.mark("yyc:chunk:reply");
} catch {
  // performance marks are for load-timing only
}

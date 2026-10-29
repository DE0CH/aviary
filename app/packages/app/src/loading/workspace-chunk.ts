// The conversation page, one chunk: the workspace screen with its header, tabs row, panels (the chat panel and its
// composer, files, terminals, setup) and git actions (loading/stages.ts, docs/paseo-ui.md "Loading"). The new-chat
// screen, the landing page, does without it; a conversation page fetches it at once, the rest with the reply chunk.
export { WorkspaceScreen } from "@/screens/workspace/workspace-screen";

try {
  performance.mark("yyc:chunk:workspace");
} catch {
  // performance marks are for load-timing only
}

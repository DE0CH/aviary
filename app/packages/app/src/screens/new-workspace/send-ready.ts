/**
 * Whether the new-chat composer can send: connected, the host's workspace list (and so its
 * projects) has arrived, and a project is chosen when the host has any. Before that a send would
 * be refused ("Choose a project"); the send button stays off instead (docs/paseo-ui.md "Loading").
 * A host with no project at all can still send, so its own error explains what is missing.
 */
export function isNewWorkspaceSendReady(input: {
  clientReady: boolean;
  hasHydratedWorkspaces: boolean;
  hasProjectOptions: boolean;
  hasSelectedSourceDirectory: boolean;
}): boolean {
  if (!input.clientReady || !input.hasHydratedWorkspaces) return false;
  return !input.hasProjectOptions || input.hasSelectedSourceDirectory;
}

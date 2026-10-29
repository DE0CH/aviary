// What the app shows on request or later, one chunk mounted at stage 3 (loading/stages.ts, docs/paseo-ui.md
// "Loading"): the dialogs and sheets (the host chooser and host confirmation, command center with its actions, add project, provider settings, workspace setup
// and rename, keyboard shortcuts, diagnostics, changelog, YYC's conversation rename/share/delete), the download toast, the sidebar's callouts (desktop update,
// Rosetta, worktree setup) and the legacy skills migration. Each reads a store, so a request made before the chunk
// arrives shows once it has.
import { LegacyAgentSkillsMigration } from "@/agent-skills/legacy-migration";
import { ChangelogHost } from "@/changelog";
import { CommandCenter } from "@/command-center/command-center";
import { CommandCenterRootActions } from "@/command-center/root-registration";
import { CommandCenterWorkspaceActions } from "@/command-center/workspace-registration";
import { PluginCommandCenterActions } from "@/plugins/command-center/registration";
import { AddProjectFlowHost } from "@/components/add-project-flow-host";
import { AppDiagnosticHost } from "@/components/app-diagnostic-host";
import { DownloadToast } from "@/components/download-toast";
import { KeyboardShortcutsDialog } from "@/components/keyboard-shortcuts-dialog";
import { ProviderSettingsHost } from "@/components/provider-settings-host";
import { WorkspaceRenameHost } from "@/components/workspace-rename-host";
import { WorkspaceSetupDialog } from "@/components/workspace-setup-dialog";
import { WorktreeSetupCalloutSource } from "@/components/worktree-setup-callout-source";
import { RosettaCalloutSource } from "@/desktop/updates/rosetta-callout-source";
import { UpdateCalloutSource } from "@/desktop/updates/update-callout-source";
import { ConversationDialogsHost } from "@/yyc/conversation-dialogs";
import { HostChooserModal } from "@/hosts/host-chooser-modal";
import { HostConfirmationSheet } from "@/hosts/host-confirmation-sheet";

export function AppHosts() {
  return (
    <>
      <HostChooserModal />
      <HostConfirmationSheet />
      <DownloadToast />
      <RosettaCalloutSource />
      <UpdateCalloutSource />
      <LegacyAgentSkillsMigration />
      <WorktreeSetupCalloutSource />
      <CommandCenterRootActions />
      <CommandCenterWorkspaceActions />
      <PluginCommandCenterActions />
      <WorkspaceRenameHost />
      <CommandCenter />
      <AddProjectFlowHost />
      <ProviderSettingsHost />
      <WorkspaceSetupDialog />
      <KeyboardShortcutsDialog />
      <AppDiagnosticHost />
      <ChangelogHost />
      <ConversationDialogsHost />
    </>
  );
}

try {
  performance.mark("yyc:chunk:hosts");
} catch {
  // performance marks are for load-timing only
}

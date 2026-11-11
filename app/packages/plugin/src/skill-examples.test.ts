import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";
import { describe, expect, it } from "vitest";

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../..");
// Checks the example as a plugin client file, with the same options as the plugin examples.
function typeErrors(code: string): string[] {
  const configPath = path.join(repository, "packages/plugin/tsconfig.examples.json");
  const { config } = ts.readConfigFile(configPath, ts.sys.readFile);
  const { options } = ts.parseJsonConfigFileContent(config, ts.sys, path.dirname(configPath));
  const file = path.join(repository, "plugin-examples/skill-example/client/example.tsx");
  const host = ts.createCompilerHost(options);
  const getSourceFile = host.getSourceFile;
  host.getSourceFile = (name, language, ...rest) =>
    path.resolve(name) === file
      ? ts.createSourceFile(name, code, language)
      : getSourceFile.call(host, name, language, ...rest);
  host.fileExists = (name) => path.resolve(name) === file || ts.sys.fileExists(name);
  const program = ts.createProgram([file], options, host);
  return ts
    .getPreEmitDiagnostics(program)
    .map((diagnostic) => ts.flattenDiagnosticMessageText(diagnostic.messageText, "\n"));
}

it("keeps historical string descriptor helpers while accepting translated registrations", () => {
  expect(
    typeErrors(`
    import { defineTranslations, type PluginText, type PluginFilePanelContribution, type PluginClientContext, type PluginScreenContribution, type PluginWorkspacePanelContribution, type PluginSettingsScreenContribution, type PluginSidebarItemContribution, type PluginSidebarContribution, type PluginCommandCenterItemContribution, type PluginClientSlashCommandContribution, type PluginButton, type PluginHeaderButtonContribution, type PluginComposerPillContribution, type PluginButtonMenuEntry } from "@getpaseo/plugin/client";
    import { z } from "zod";
    import { defineAttachmentSource, type PluginThemeContribution, type PluginAttachmentSourceContribution } from "@getpaseo/plugin";
    const messages = defineTranslations({ resources: { en: { title: "Notes" } } });
    const Component = () => null;
    function historicalScreen(screen: PluginScreenContribution) {
      return typeof screen.title === "function" ? screen.title({}).trim() : screen.title.trim();
    }
    function historicalLabels(filePanel: PluginFilePanelContribution, panel: PluginWorkspacePanelContribution, settings: PluginSettingsScreenContribution, sidebar: PluginSidebarItemContribution, legacy: PluginSidebarContribution, command: PluginCommandCenterItemContribution, slash: PluginClientSlashCommandContribution, button: PluginButton, header: PluginHeaderButtonContribution, pill: PluginComposerPillContribution, menu: PluginButtonMenuEntry, theme: PluginThemeContribution, source: PluginAttachmentSourceContribution) {
      filePanel.title.trim(); panel.title.trim(); settings.title.trim(); sidebar.title.trim(); legacy.title.trim(); command.title.trim();
      slash.description.trim(); slash.argumentHint.trim(); button.title.trim(); button.label?.trim();
      header.button.title.trim(); pill.button.title.trim(); if (menu.kind === "item") menu.title.trim();
      theme.name.trim(); source.title.trim(); source.pickerTitle.trim(); source.searchPlaceholder.trim();
    }
    function contribute(client: PluginClientContext) {
      const oldScreen: PluginScreenContribution = { id: "old", title: (params) => params.name ?? "Notes", Component };
      historicalScreen(oldScreen);
      client.addScreen(oldScreen);
      client.addScreen({ id: "translated", title: messages.message("title"), Component });
      client.addScreen({ id: "callback", title: (params) => messages.message("title"), Component });
      const translated: PluginScreenContribution<PluginText> = { id: "typed", title: messages.message("title"), Component };
      client.addScreen(translated);
      client.addSidebarHeaderItem({ id: "notes", title: messages.message("title"), Component });
      client.addFilePanel({ id: "notes", title: messages.message("title"), icon: "File", files: { extensions: ["notes"] }, Component });
      client.addSettingsScreen({ id: "notes", title: messages.message("title"), icon: "File", Component });
      client.addWorkspacePanel({ id: "notes", title: messages.message("title"), icon: "File", context: "workspace", Component });
      client.addCommandCenterItem({ id: "notes", title: messages.message("title"), icon: "File", context: "global", onSelect() {} });
      client.addSlashCommand({ name: "notes", description: messages.message("title"), argumentHint: "", context: "workspace", onSubmit() {} });
      const button = client.addHeaderButton({ id: "notes", workspaceId: "workspace", button: { title: messages.message("title"), icon: "File", behavior: { kind: "menu", items: [{ kind: "item", id: "open", title: messages.message("title"), behavior: { kind: "action", onPress() {} } }] } } });
      button.update({ label: messages.message("title") });
      const source = defineAttachmentSource({ id: "source", title: messages.message("title"), pickerTitle: messages.message("title"), searchPlaceholder: messages.message("title"), icon: "File", search: { name: "source.search", input: z.object({}), output: z.object({}) } });
      client.addAttachmentSource(source);
      // @ts-expect-error a translated reference is never typed as a string
      source.title.trim();
    }
  `),
  ).toEqual([]);
});

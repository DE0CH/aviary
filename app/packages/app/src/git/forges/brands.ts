import type { ClientForgeBrand } from "@/git/client-forge-module";
import { codebergForgeView } from "./codeberg.view";
import { forgejoForgeView } from "./forgejo.view";
import { giteaForgeView } from "./gitea.view";
import { githubForgeView } from "./github.view";
import { gitlabForgeBrand } from "./gitlab.brand";

/**
 * Every forge's brand mark and colour, without the PR-pane contributions (forges/view.ts): the icon lookups
 * (git/forge-icon.tsx) need only these, and the composer draws them, so they stay out of the pane's code
 * (docs/paseo-ui.md "Loading"). Keep in step with CLIENT_FORGE_VIEW_MODULES (a test checks).
 */
export const CLIENT_FORGE_BRANDS: readonly ClientForgeBrand[] = [
  githubForgeView,
  gitlabForgeBrand,
  giteaForgeView,
  forgejoForgeView,
  codebergForgeView,
];

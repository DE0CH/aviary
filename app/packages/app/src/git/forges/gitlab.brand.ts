import { GitLabIcon } from "@/components/icons/gitlab-icon";
import type { ClientForgeBrand } from "@/git/client-forge-module";

/** GitLab's mark and colour, apart from its PR-pane contributions (gitlab.view.tsx), for icon lookups. */
export const gitlabForgeBrand = {
  id: "gitlab",
  icon: GitLabIcon,
  brandColor: {
    light: "#FC6D26",
    dark: "#FC6D26",
  },
} satisfies ClientForgeBrand;

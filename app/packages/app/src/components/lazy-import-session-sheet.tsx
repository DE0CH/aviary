import { lazySheet } from "@/components/lazy-area";
import type { ImportSessionSheetProps } from "@/components/import-session-sheet";

// The import-session sheet is its own chunk, fetched the first time it opens (docs/paseo-ui.md "Loading")
export const ImportSessionSheet = lazySheet<
  typeof import("@/components/import-session-sheet"),
  ImportSessionSheetProps
>(
  "import-session",
  () => import("@/components/import-session-sheet"),
  (m) => m.ImportSessionSheet
);

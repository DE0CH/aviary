// YYC AI's wording on top of Paseo's translations (DE0CH/claudecodeui docs/paseo-ui.md). Paseo names things
// for developers (workspaces, agents, projects); staff see conversations with YYC AI. Only the strings that differ
// live here; everything else is Paseo's own text.
import type { TranslationResources } from "./en";

type DeepPartial<T> = { [K in keyof T]?: T[K] extends object ? DeepPartial<T[K]> : T[K] };

export type TranslationOverrides = DeepPartial<TranslationResources>;

export const enOverrides: TranslationOverrides = {
  composer: {
    placeholders: {
      desktop: "Message YYC AI",
      mobile: "Message YYC AI",
      fallback: "Message YYC AI",
    },
  },
  newWorkspace: {
    title: "How can I help?",
    create: "Send",
  },
  sidebar: {
    actions: { newWorkspace: "New chat" },
    workspace: { actions: { newWorkspace: "New chat", archive: "Delete", archiving: "Deleting…" } },
    sections: { sessions: "Recently deleted" },
  },
  sessions: {
    title: "Recently deleted",
    empty: "Nothing deleted in the last 30 days",
    noMatches: "No deleted conversations match",
    searchPlaceholder: "Search recently deleted",
  },
};

export const zhHKOverrides: TranslationOverrides = {
  composer: {
    placeholders: {
      desktop: "輸入訊息",
      mobile: "輸入訊息",
      fallback: "輸入訊息",
    },
  },
  newWorkspace: {
    title: "有什麼可以幫你？",
    create: "傳送",
  },
  sidebar: {
    actions: { newWorkspace: "新對話" },
    workspace: { actions: { newWorkspace: "新對話", archive: "刪除", archiving: "正在刪除…" } },
    sections: { sessions: "最近刪除" },
  },
  sessions: {
    title: "最近刪除",
    empty: "過去 30 日沒有刪除的對話",
    noMatches: "沒有符合的已刪除對話",
    searchPlaceholder: "搜尋最近刪除",
  },
};

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

export function withOverrides<T>(base: T, overrides: TranslationOverrides): T {
  if (!isPlainObject(base) || !isPlainObject(overrides)) return base;
  const merged: Record<string, unknown> = { ...base };
  for (const [key, value] of Object.entries(overrides)) {
    merged[key] = isPlainObject(value) && isPlainObject(merged[key])
      ? withOverrides(merged[key], value as TranslationOverrides)
      : value;
  }
  return merged as T;
}

/** Every remaining mention of Paseo in the text names the product staff use instead. */
export function withYycBrand<T>(value: T): T {
  if (typeof value === "string") return value.replaceAll("Paseo", "YYC AI") as T;
  if (Array.isArray(value)) return value.map(withYycBrand) as T;
  if (!isPlainObject(value)) return value;
  return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, withYycBrand(item)])) as T;
}

// Text the tools role shows staff (the confirmation card's summary), in the instance's locale (config product.locale,
// the same value the UI starts in): zh-HK (Traditional Chinese, Hong Kong usage) or English. Results for the model stay
// English.
export type ToolsLocale = "zh-HK" | "en";

export const toolsLocale = (l: string | undefined | null): ToolsLocale => (String(l ?? "").toLowerCase().startsWith("zh") ? "zh-HK" : "en");

const STRINGS = {
  "zh-HK": {
    networkAccess: (host: string) => `允許沙盒連接 ${host}？`,
    tool: (title: string, args: string) => `執行「${title}」：${args}`,
  },
  en: {
    networkAccess: (host: string) => `Allow the sandbox to reach ${host}?`,
    tool: (title: string, args: string) => `${title}: ${args}`,
  },
} satisfies Record<ToolsLocale, unknown>;

export const confirmText = (locale: string | undefined | null) => STRINGS[toolsLocale(locale)];

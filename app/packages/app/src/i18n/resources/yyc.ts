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
    input: {
      accessibilityLabel: "Message",
      interruptAgent: "Stop",
      sendAndInterrupt: "Send",
      sendAndSteer: "Send",
      sendMessage: "Send",
    },
    cancel: { cancelingAgent: "Stopping", stopAgent: "Stop", interrupt: "Stop" },
    errors: { failedToCreateAgent: "Couldn't start the conversation", noHostSelected: "Not connected" },
  },
  agentStream: { empty: "Send a message to start" },
  agentPanel: {
    states: {
      notFound: "This conversation can't be found",
      failedToLoad: "Couldn't load the conversation",
      reconnecting: "Reconnecting…",
      timelineSyncFailed: "Couldn't refresh the conversation.",
      archivingTitle: "Deleting the conversation…",
      archivingSubtitle: "Deleting the conversation…",
    },
    unavailable: {
      unknownHost: "This conversation can't be opened here.",
      addHost: "Go back to your conversations and try again.",
      preparingSession: "Connecting…",
      connecting: "Connecting…",
      showSoon: "The conversation will show in a moment.",
      showWhenOnline: "The conversation will show once the connection is back.",
      reconnectingTo: "Reconnecting…",
      showAgainWhenReachable: "The conversation will show again once the connection is back.",
    },
    archived: { callout: "This conversation is deleted", unarchive: "Restore" },
  },
  shell: {
    commandCenter: {
      placeholder: "Search conversations",
      workspaces: "Conversations",
      noMatches: "No conversations match",
    },
  },
  workspace: {
    header: {
      actions: {
        workspaceActions: "Conversation options",
        workspaceActionsUnhealthy: "Conversation options",
      },
    },
  },
  settings: {
    general: {
      chatDisplay: {
        description: "How much of YYC AI's working steps the chat shows",
        options: {
          full: "Every step",
          collapseToolCalls: "Fold working steps",
          collapseTurns: "Fold earlier replies",
        },
      },
    },
    appearance: {
      theme: { options: { light: "Light", dark: "Dark", auto: "System" } },
      chatOutline: { description: "Show an outline for jumping between your questions" },
    },
  },
  newWorkspace: {
    title: "How can I help?",
    create: "Send",
  },
  sidebar: {
    actions: { newWorkspace: "New chat" },
    workspace: {
      actions: {
        newWorkspace: "New chat",
        archive: "Delete",
        archiving: "Deleting…",
        rename: "Rename",
        menu: "Conversation options",
      },
      rename: { title: "Rename conversation" },
    },
    sections: { sessions: "Recently deleted" },
  },
  sessions: {
    title: "Recently deleted",
    loadFailed: "Couldn't load recently deleted conversations",
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
    attachments: {
      addFile: "上載檔案",
      dropFilesHere: "將檔案拖放到這裏",
      removeFile: "移除檔案",
    },
    errors: {
      uploadFailed: "上載檔案失敗",
      fileTooLarge: "{{fileName}} 太大（上限 {{size}}）",
      failedToCreateAgent: "未能開始對話",
      noHostSelected: "未有連線",
    },
    input: {
      accessibilityLabel: "輸入訊息",
      interruptAgent: "停止",
      sendAndInterrupt: "傳送",
      sendAndSteer: "傳送",
      sendMessage: "傳送",
    },
    cancel: { cancelingAgent: "正在停止", stopAgent: "停止", interrupt: "停止" },
  },
  agentStream: { empty: "輸入訊息開始對話" },
  agentPanel: {
    states: {
      notFound: "找不到此對話",
      failedToLoad: "未能載入對話",
      reconnecting: "正在重新連線…",
      timelineSyncFailed: "未能更新對話。",
      archivingTitle: "正在刪除對話…",
      archivingSubtitle: "正在刪除對話…",
    },
    unavailable: {
      unknownHost: "未能在這裏開啟此對話。",
      addHost: "請返回對話列表再試。",
      preparingSession: "正在連線…",
      connecting: "正在連線…",
      showSoon: "對話稍後會顯示。",
      showWhenOnline: "連線恢復後會顯示對話。",
      reconnectingTo: "正在重新連線…",
      showAgainWhenReachable: "連線恢復後會再顯示對話。",
    },
    archived: { callout: "此對話已刪除", unarchive: "還原" },
  },
  shell: {
    commandCenter: {
      placeholder: "搜尋對話",
      workspaces: "對話",
      noMatches: "沒有符合的對話",
    },
  },
  workspace: {
    header: {
      actions: {
        workspaceActions: "對話選項",
        workspaceActionsUnhealthy: "對話選項",
      },
    },
  },
  settings: {
    general: {
      chatDisplay: {
        description: "聊天中顯示多少 YYC AI 的工作步驟",
        options: {
          full: "顯示全部步驟",
          collapseToolCalls: "收起工作步驟",
          collapseTurns: "收起之前的回覆",
        },
      },
    },
    appearance: {
      theme: { options: { light: "淺色", dark: "深色", auto: "跟隨系統" } },
      chatOutline: { description: "顯示大綱，在你的提問之間跳轉" },
    },
  },
  newWorkspace: {
    title: "有什麼可以幫你？",
    create: "傳送",
  },
  sidebar: {
    actions: { newWorkspace: "新對話" },
    workspace: {
      actions: {
        newWorkspace: "新對話",
        archive: "刪除",
        archiving: "正在刪除…",
        rename: "重新命名",
        menu: "對話選項",
      },
      rename: { title: "重新命名對話" },
    },
    sections: { sessions: "最近刪除" },
  },
  sessions: {
    title: "最近刪除",
    loadFailed: "未能載入最近刪除的對話",
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

// UI strings: zh-HK (Traditional Chinese, Hong Kong usage) and English. The instance's locale (GET /api/me) is the
// default; a person's own choice is remembered in this browser only.
export type Locale = "zh-HK" | "en";

const zh = {
  newChat: "新對話",
  untitled: "新對話",
  chats: "對話",
  noChats: "未有對話",
  rename: "重新命名",
  delete: "刪除",
  confirmDelete: "確定刪除？",
  cancel: "取消",
  save: "儲存",
  menu: "選單",
  closeMenu: "關閉選單",
  language: "語言",
  model: "模型",
  composerPlaceholder: "輸入訊息…",
  send: "傳送",
  stop: "停止",
  attach: "附加檔案",
  removeAttachment: "移除附件",
  uploading: "上載中…",
  uploadFailed: "上載失敗",
  copy: "複製",
  copied: "已複製",
  thinking: "思考中…",
  working: "處理中…",
  queued: "排隊中…",
  stopped: "已停止",
  failed: "出現錯誤",
  budgetExhausted: (limit: string, owner: string | null) => `${owner || "管理員"} 設定了 ${limit} 美元的使用上限，目前已用完。`,
  retry: "再試",
  files: "檔案",
  download: "下載",
  sandboxReset: "工作環境已重設：檔案已還原，之前安裝的軟件和背景程序已經停止。",
  sandboxStarting: "正在準備工作環境…",
  notAllowedTitle: "未有使用權限",
  notAllowedBody: "你的帳戶未在使用名單上。請聯絡 Deyao 加入。",
  loadFailed: "載入失敗，請重新整理頁面。",
  offline: "連線中斷，正在重新連接…",
  sendFailed: "未能傳送，請再試。",
  busy: "上一條訊息仍在處理中。",
  input: "輸入",
  output: "結果",
  showMore: "顯示全部",
  error: "錯誤",
  you: "你",
  conversationOptions: "對話選項",
  openFile: "開啟",
  tool: {
    web_search: "搜尋網絡",
    news_search: "搜尋慧科新聞",
    social_search: "搜尋社交平台",
    web_fetch: "讀取網頁",
    bash: "執行指令",
    read_file: "讀取檔案",
    write_file: "寫入檔案",
    edit_file: "修改檔案",
    glob_search: "尋找檔案",
    grep_search: "搜尋檔案內容",
    yyc: "查詢新聞監察數據",
    other: "使用工具",
  },
};

type Strings = typeof zh;

const en: Strings = {
  newChat: "New chat",
  untitled: "New chat",
  chats: "Chats",
  noChats: "No chats yet",
  rename: "Rename",
  delete: "Delete",
  confirmDelete: "Delete it?",
  cancel: "Cancel",
  save: "Save",
  menu: "Menu",
  closeMenu: "Close menu",
  language: "Language",
  model: "Model",
  composerPlaceholder: "Write a message…",
  send: "Send",
  stop: "Stop",
  attach: "Attach a file",
  removeAttachment: "Remove attachment",
  uploading: "Uploading…",
  uploadFailed: "Upload failed",
  copy: "Copy",
  copied: "Copied",
  thinking: "Thinking…",
  working: "Working…",
  queued: "Queued…",
  stopped: "Stopped",
  failed: "Something went wrong",
  budgetExhausted: (limit: string, owner: string | null) => `${owner || "The administrator"} has set a USD ${limit} budget limit and it has now been used up.`,
  retry: "Retry",
  files: "Files",
  download: "Download",
  sandboxReset: "The workspace was reset: your files are restored; earlier installs and background programs have stopped.",
  sandboxStarting: "Preparing the workspace…",
  notAllowedTitle: "Not on the list yet",
  notAllowedBody: "Your account isn't on the access list. Please ask Deyao to add you.",
  loadFailed: "Couldn't load. Please refresh the page.",
  offline: "Connection lost, reconnecting…",
  sendFailed: "Couldn't send. Please try again.",
  busy: "The previous message is still being worked on.",
  input: "Input",
  output: "Result",
  showMore: "Show all",
  error: "Error",
  you: "You",
  conversationOptions: "Chat options",
  openFile: "Open",
  tool: {
    web_search: "Searching the web",
    news_search: "Searching Wisers news",
    social_search: "Searching social platforms",
    web_fetch: "Reading a page",
    bash: "Running a command",
    read_file: "Reading a file",
    write_file: "Writing a file",
    edit_file: "Editing a file",
    glob_search: "Finding files",
    grep_search: "Searching in files",
    yyc: "Checking the news monitor",
    other: "Using a tool",
  },
};

const tables: Record<Locale, Strings> = { "zh-HK": zh, en };

let current: Locale = "zh-HK";

export function setLocale(l: Locale): void {
  current = l;
  document.documentElement.lang = l === "en" ? "en" : "zh-HK";
}

export const locale = (): Locale => current;
export const t = (): Strings => tables[current];

export function normaliseLocale(s: string | undefined | null): Locale {
  return s && s.toLowerCase().startsWith("en") ? "en" : "zh-HK";
}

const KEY = "chat.locale";
export function savedLocale(): Locale | null {
  try {
    const v = localStorage.getItem(KEY);
    return v === "en" || v === "zh-HK" ? v : null;
  } catch {
    return null;
  }
}
export function saveLocale(l: Locale): void {
  try {
    localStorage.setItem(KEY, l);
  } catch {
    /* private mode: the choice lasts for this page only */
  }
}

export function relativeTime(iso: string): string {
  const d = new Date(iso);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  const lang = current === "en" ? "en-HK" : "zh-HK";
  if (sameDay) return d.toLocaleTimeString(lang, { hour: "2-digit", minute: "2-digit" });
  const days = (now.getTime() - d.getTime()) / 86_400_000;
  if (days < 6) return d.toLocaleDateString(lang, { weekday: "short" });
  return d.toLocaleDateString(lang, { month: "short", day: "numeric" });
}

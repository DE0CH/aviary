/**
 * The words and readings of schedules (yyc/schedules.tsx): a fired message, the AI's API calls, times and outcomes.
 * No React Native here, so the unit tests run without it.
 */
export interface ScheduleRun {
  due: string;
  at: string;
  outcome: string;
  detail?: string;
}

export interface Schedule {
  id: string;
  conversationId: string;
  kind: "once" | "cron" | string;
  prompt: string;
  at?: string;
  cron?: string;
  tz: string;
  until?: string;
  describe: string;
  next: string | null;
  state: string;
  createdBy: string;
  createdAt: string;
  runs?: ScheduleRun[];
}

// ---- reading what the server and the AI wrote ----

/** A fired schedule's message: `<scheduled-message rule="…">prompt</scheduled-message>` → its rule and prompt. */
export function parseScheduledMessage(text: string | null | undefined): { rule: string; prompt: string } | null {
  if (typeof text !== "string") return null;
  const m = /^\s*<scheduled-message rule="([^"]*)">([\s\S]*)<\/scheduled-message>\s*$/.exec(text);
  if (!m) return null;
  const rule = m[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&");
  return { rule, prompt: m[2] };
}

/** What a schedules API call did, read from the AI's shell call and its output; null when it is not one (yet). */
export function readScheduleCall(
  command: string | null | undefined,
  output: string | null | undefined,
): { action: "armed" | "changed" | "deleted"; schedule: Schedule | null } | null {
  if (typeof command !== "string" || !/schedules/.test(command)) return null;
  if (!/\/v1\/sandbox\/schedules|JARVIS_ROUTE_SCHEDULES_URL|\/x\/schedules|__jarvis\/schedules|\$B\b|\$\{B\}/.test(command)) {
    return null;
  }
  const method = /-X\s*['"]?(POST|PATCH|DELETE)\b/i.exec(command)?.[1]?.toUpperCase() ?? (/(--data|-d\s|--json)/.test(command) ? "POST" : "GET");
  if (method === "GET") return null;
  if (method === "DELETE") return typeof output === "string" ? { action: "deleted", schedule: null } : null;
  const schedule = lastSchedule(output);
  if (!schedule) return null;
  return { action: method === "PATCH" ? "changed" : "armed", schedule };
}

/** The last JSON object in `output` that is a schedule (curl prints it; other text may surround it). */
function lastSchedule(output: string | null | undefined): Schedule | null {
  if (typeof output !== "string") return null;
  for (let end = output.lastIndexOf("}"); end >= 0; end = output.lastIndexOf("}", end - 1)) {
    for (let start = output.lastIndexOf("{", end); start >= 0; start = output.lastIndexOf("{", start - 1)) {
      try {
        const v = JSON.parse(output.slice(start, end + 1)) as Partial<Schedule>;
        if (v && typeof v.describe === "string" && typeof v.id === "string" && "next" in v) return v as Schedule;
      } catch {
        // not a whole object: widen
      }
    }
  }
  return null;
}

// ---- times ----

export const WEEKDAY = ["日", "一", "二", "三", "四", "五", "六"];

interface LocalParts {
  y: number;
  m: number;
  d: number;
  hm: string;
}

/** The local date and time a server time names (`2026-11-07T08:30:00+08:00`: already in the schedule's zone). */
function partsOf(iso: string): LocalParts | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(iso);
  return m ? { y: Number(m[1]), m: Number(m[2]), d: Number(m[3]), hm: `${m[4]}:${m[5]}` } : null;
}

function weekday(p: LocalParts): string {
  return WEEKDAY[new Date(Date.UTC(p.y, p.m - 1, p.d)).getUTCDay()];
}

function todayIn(tz: string): LocalParts {
  let s: string;
  try {
    s = new Intl.DateTimeFormat("en-CA", { timeZone: tz, year: "numeric", month: "2-digit", day: "2-digit" }).format(new Date());
  } catch {
    s = new Date().toISOString().slice(0, 10);
  }
  const [y, m, d] = s.split("-").map(Number);
  return { y, m, d, hm: "" };
}

function dayDiff(a: LocalParts, b: LocalParts): number {
  return Math.round((Date.UTC(a.y, a.m - 1, a.d) - Date.UTC(b.y, b.m - 1, b.d)) / 86_400_000);
}

/** The next time in words: 今天/明天 08:30, 星期一 15:00 within a week, else 11月20日（星期五）08:30. */
export function nextLabel(iso: string | null | undefined, tz: string): string {
  if (!iso) return "沒有";
  const p = partsOf(iso);
  if (!p) return iso;
  const diff = dayDiff(p, todayIn(tz));
  if (diff === 0) return `今天 ${p.hm}`;
  if (diff === 1) return `明天 ${p.hm}`;
  if (diff > 1 && diff < 7) return `星期${weekday(p)} ${p.hm}`;
  return `${p.m}月${p.d}日（星期${weekday(p)}）${p.hm}`;
}

/** A past time: 11月6日 08:30. */
export function shortLabel(iso: string | null | undefined): string {
  const p = iso ? partsOf(iso) : null;
  return p ? `${p.m}月${p.d}日 ${p.hm}` : "";
}

export const ZONE_WORDS: Record<string, string> = {
  "Asia/Hong_Kong": "香港時間", "Asia/Shanghai": "北京時間", "Asia/Taipei": "台北時間", "Asia/Macau": "澳門時間",
  "Asia/Singapore": "新加坡時間", "Asia/Tokyo": "東京時間", "Asia/Seoul": "首爾時間", "Europe/London": "倫敦時間",
  "America/New_York": "紐約時間", UTC: "UTC", "Etc/UTC": "UTC",
};

export const OUTCOME_WORDS: Record<string, string> = {
  sent: "已送出",
  sent_mid_turn: "已送出（當時對話正在工作）",
  pending: "送出中（對話正在工作）",
  redeliver: "稍後送出",
  skipped_late: "服務暫停，已略過",
  skipped_paused: "全部排程已暫停，已略過",
  skipped_deleted: "對話已刪除，已略過",
  failed: "未能送出",
};


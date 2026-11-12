/**
 * The activity panel's words and arithmetic (yyc/activity.tsx): what a conversation runs beside its reply, as
 * GET /api/jarvis/sessions/<id>/tasks lists it (aviary magpie's `tasks`).
 */

export type TaskStatus = "running" | "completed" | "failed" | "stopped";
export type TaskType = "command" | "subagent" | "workflow" | "monitor" | "other";

export interface ActivityTask {
  taskId: string;
  toolUseId?: string | null;
  type: TaskType;
  /** What it was started to do. */
  description?: string | null;
  status: TaskStatus;
  startedAt: string;
  endedAt?: string | null;
  /** What it is doing now (its latest progress), while it runs. */
  now?: string | null;
  /** How many progress reports it has made (a subagent: its tool calls). */
  steps?: number;
  /** Its result. */
  summary?: string | null;
}

export type ActivityLocale = "zh" | "en";

/** Chinese for a Chinese interface, English for every other. */
export function activityLocale(language: string | undefined): ActivityLocale {
  return (language ?? "").toLowerCase().startsWith("zh") ? "zh" : "en";
}

const WORDS = {
  en: {
    title: "Activity",
    running: (n: number) => `${n} running`,
    idle: "Activity",
    types: { command: "Command", subagent: "Subagent", workflow: "Workflow", monitor: "Monitor", other: "Task" },
    stop: "Stop",
    stopping: "Stopping",
    runningFor: "Running for",
    finishedIn: "Finished in",
    failedAfter: "Failed after",
    stoppedAfter: "Stopped after",
    now: "Now",
    steps: "Steps",
    result: "Result",
    none: "Nothing has run beside this conversation's replies.",
    footer: "Finished ones stay here for the rest of the conversation.",
    back: "Back",
    close: "Close",
    untitled: "Task",
  },
  zh: {
    title: "活動",
    running: (n: number) => `${n} 個進行中`,
    idle: "活動",
    types: { command: "指令", subagent: "子代理", workflow: "工作流程", monitor: "監察", other: "工作" },
    stop: "停止",
    stopping: "正在停止",
    runningFor: "已執行",
    finishedIn: "完成，用時",
    failedAfter: "失敗，用時",
    stoppedAfter: "已停止，用時",
    now: "目前",
    steps: "步驟",
    result: "結果",
    none: "這個對話沒有在回覆以外執行過工作。",
    footer: "完成的工作會留在這裡，直到對話結束。",
    back: "返回",
    close: "關閉",
    untitled: "工作",
  },
} as const;

export function activityWords(locale: ActivityLocale) {
  return WORDS[locale];
}

/** `m:ss`, or `h:mm:ss` from an hour on; nothing negative. */
export function elapsedLabel(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  const ss = String(s).padStart(2, "0");
  return h > 0 ? `${h}:${String(m).padStart(2, "0")}:${ss}` : `${m}:${ss}`;
}

/** How long a task has run (until `now`) or ran (until it ended); 0 for times that do not parse. */
export function taskElapsedMs(task: Pick<ActivityTask, "startedAt" | "endedAt">, now: number): number {
  const start = Date.parse(task.startedAt);
  const end = task.endedAt ? Date.parse(task.endedAt) : now;
  if (Number.isNaN(start) || Number.isNaN(end)) return 0;
  return Math.max(0, end - start);
}

/** The label before a task's time: running, or how it ended. */
export function elapsedHeading(status: TaskStatus, locale: ActivityLocale): string {
  const w = WORDS[locale];
  if (status === "running") return w.runningFor;
  if (status === "failed") return w.failedAfter;
  if (status === "stopped") return w.stoppedAfter;
  return w.finishedIn;
}

export function runningCount(tasks: readonly ActivityTask[]): number {
  return tasks.filter((t) => t.status === "running").length;
}

/** The chip's text: how many run, or the panel's name when none does. */
export function chipLabel(tasks: readonly ActivityTask[], locale: ActivityLocale): string {
  const n = runningCount(tasks);
  return n > 0 ? WORDS[locale].running(n) : WORDS[locale].idle;
}

/** A task's title: what it was started to do, else what it is. */
export function taskTitle(task: ActivityTask, locale: ActivityLocale): string {
  const d = task.description?.trim();
  return d ? d : WORDS[locale].types[task.type] ?? WORDS[locale].untitled;
}

/** What Claude Code's progress line says without its own "Running " prefix. */
export function nowLabel(now: string | null | undefined): string | null {
  const text = now?.trim();
  if (!text) return null;
  return text.replace(/^Running\s+/, "");
}

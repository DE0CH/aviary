/**
 * Activity (Deyao, 2026-11-12; design: the approved mock): what a conversation runs beside its reply — a command in
 * the background, a subagent, a workflow, a monitor — shown the way its schedules are (yyc/schedules.tsx):
 *   - a chip in the conversation's header, 「n running」 with a still icon (ActivityChip; nothing when the conversation
 *     never started one), opening
 *   - a panel beside the chat that pushes it narrower (full screen with a back arrow on a phone; ActivityPanel): one
 *     section per task with what it is, how long it has run, what it does now, its steps, and Stop; the finished
 *     ones below, with their result. Nothing in it moves except the clock.
 * The panel reads GET /api/jarvis/sessions/<id>/tasks and stops one with POST …/tasks/<taskId>/stop (aviary's
 * pigeon). A server without the route has no chip.
 */
import React, { useCallback, useEffect, useState } from "react";
import { Pressable, ScrollView, Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { ChevronLeft, ListTodo, X } from "lucide-react-native";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { create } from "zustand";
import { api } from "@/yyc/api";
import { useViewAsReadOnly } from "@/yyc/view-as";
import type { Theme } from "@/styles/theme";
import {
  type ActivityLocale,
  type ActivityTask,
  activityLocale,
  activityWords,
  chipLabel,
  elapsedHeading,
  elapsedLabel,
  nowLabel,
  runningCount,
  taskElapsedMs,
  taskTitle,
} from "@/yyc/activity-text";

// ---- data ----

const queryKey = (sessionId: string) => ["yyc-activity", sessionId] as const;

/** While something runs, or the panel is open, the list is read this often; otherwise that often. */
const BUSY_REFRESH_MS = 4_000;
const IDLE_REFRESH_MS = 20_000;

/** The conversation's tasks, the running ones first (the server's order). */
export function useConversationTasks(sessionId: string | null, watching: boolean) {
  return useQuery({
    queryKey: queryKey(sessionId ?? ""),
    enabled: !!sessionId,
    queryFn: async () =>
      (await api<{ tasks?: ActivityTask[] }>(`/api/jarvis/sessions/${encodeURIComponent(sessionId ?? "")}/tasks`))
        .tasks ?? [],
    // a server without the route answers 404: no chip, and it is not asked again and again
    retry: false,
    refetchInterval: (query) => {
      if (query.state.status === "error") return false;
      const running = runningCount(query.state.data ?? []) > 0;
      return running || watching ? BUSY_REFRESH_MS : IDLE_REFRESH_MS;
    },
    staleTime: 2_000,
  });
}

/** Which conversation's panel is open. */
export const useActivityPanel = create<{
  openFor: string | null;
  toggle: (sessionId: string) => void;
  close: () => void;
}>((set) => ({
  openFor: null,
  toggle: (sessionId) => set((s) => ({ openFor: s.openFor === sessionId ? null : sessionId })),
  close: () => set({ openFor: null }),
}));

function useActivityLocale(): ActivityLocale {
  const { i18n } = useTranslation();
  return activityLocale(i18n.language);
}

// ---- the chip ----

const ThemedList = withUnistyles(ListTodo);
const ThemedX = withUnistyles(X);
const ThemedBack = withUnistyles(ChevronLeft);
const mutedColor = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const foregroundColor = (theme: Theme) => ({ color: theme.colors.foreground });

/** 「n running」 in the conversation's header (nothing when it never ran a task); opens the panel. */
export function ActivityChip({ sessionId }: { sessionId: string | null }) {
  const open = useActivityPanel((s) => (sessionId ? s.openFor === sessionId : false));
  const toggle = useActivityPanel((s) => s.toggle);
  const { data } = useConversationTasks(sessionId, open);
  const locale = useActivityLocale();
  const tasks = data ?? [];
  if (!sessionId || tasks.length === 0) return null;
  const label = chipLabel(tasks, locale);
  return (
    <Pressable
      testID="activity-chip"
      accessibilityRole="button"
      accessibilityState={{ expanded: open }}
      accessibilityLabel={label}
      onPress={() => toggle(sessionId)}
      style={[styles.chip, open ? styles.chipOpen : null]}
    >
      <ThemedList size={13} uniProps={mutedColor} />
      <Text style={styles.chipText}>{label}</Text>
    </Pressable>
  );
}

// ---- the panel ----

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <View style={styles.fact}>
      <Text style={styles.factLabel}>{label}</Text>
      <Text style={styles.factValue}>{value}</Text>
    </View>
  );
}

function TaskSection({
  task,
  now,
  locale,
  canStop,
  stopping,
  onStop,
}: {
  task: ActivityTask;
  now: number;
  locale: ActivityLocale;
  canStop: boolean;
  stopping: boolean;
  onStop: (taskId: string) => void;
}) {
  const w = activityWords(locale);
  const running = task.status === "running";
  const doing = running ? nowLabel(task.now) : null;
  const handleStop = useCallback(() => onStop(task.taskId), [onStop, task.taskId]);
  return (
    <View style={styles.section} testID="activity-task">
      <View style={styles.titleRow}>
        <Text style={styles.title} numberOfLines={2}>
          {taskTitle(task, locale)}
        </Text>
        <Text style={styles.tag}>{w.types[task.type] ?? w.untitled}</Text>
        {running && canStop ? (
          <Pressable
            testID="activity-stop"
            accessibilityRole="button"
            accessibilityLabel={w.stop}
            disabled={stopping}
            onPress={handleStop}
            style={styles.stop}
          >
            <Text style={styles.stopText}>{stopping ? w.stopping : w.stop}</Text>
          </Pressable>
        ) : null}
      </View>
      <View style={styles.elapsedRow}>
        <Text style={styles.elapsedLabel}>{elapsedHeading(task.status, locale)}</Text>
        <Text style={running ? styles.elapsed : styles.elapsedDone}>
          {elapsedLabel(taskElapsedMs(task, now))}
        </Text>
      </View>
      {doing ? (
        <>
          <Text style={styles.heading}>{w.now}</Text>
          <Text style={styles.mono} numberOfLines={3}>
            {doing}
          </Text>
        </>
      ) : null}
      {task.type === "subagent" && (task.steps ?? 0) > 0 ? (
        <Fact label={w.steps} value={String(task.steps)} />
      ) : null}
      {!running && task.summary ? (
        <>
          <Text style={styles.heading}>{w.result}</Text>
          <Text style={styles.result} numberOfLines={8} selectable>
            {task.summary}
          </Text>
        </>
      ) : null}
    </View>
  );
}

/**
 * The panel beside the chat (it takes its width from the chat column, which gets narrower); on a phone it covers the
 * chat, with a back arrow.
 */
export function ActivityPanel({ sessionId, compact }: { sessionId: string | null; compact: boolean }) {
  const open = useActivityPanel((s) => (sessionId ? s.openFor === sessionId : false));
  const close = useActivityPanel((s) => s.close);
  const { data } = useConversationTasks(open ? sessionId : null, open);
  const locale = useActivityLocale();
  const readOnly = useViewAsReadOnly();
  const queryClient = useQueryClient();
  const [now, setNow] = useState(() => Date.now());
  const [stopping, setStopping] = useState<ReadonlySet<string>>(() => new Set());
  const tasks = data ?? [];
  const anyRunning = runningCount(tasks) > 0;

  useEffect(() => {
    // a conversation left with its panel open: closed, so the next one starts without it
    return () => {
      if (useActivityPanel.getState().openFor === sessionId) useActivityPanel.getState().close();
    };
  }, [sessionId]);

  useEffect(() => {
    // the clock of the running tasks; nothing ticks while none runs or the panel is closed
    if (!open || !anyRunning) return;
    setNow(Date.now());
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [anyRunning, open]);

  const handleStop = useCallback(
    (taskId: string) => {
      if (!sessionId) return;
      setStopping((prev) => new Set(prev).add(taskId));
      void api(`/api/jarvis/sessions/${encodeURIComponent(sessionId)}/tasks/${encodeURIComponent(taskId)}/stop`, {
        method: "POST",
        body: "{}",
      })
        .catch(() => {
          // not stopped: the button comes back
          setStopping((prev) => {
            const next = new Set(prev);
            next.delete(taskId);
            return next;
          });
        })
        .finally(() => {
          void queryClient.invalidateQueries({ queryKey: queryKey(sessionId) });
        });
    },
    [queryClient, sessionId],
  );

  if (!open || !sessionId) return null;
  const w = activityWords(locale);
  return (
    <View style={compact ? styles.panelPhone : styles.panelDock} testID="activity-panel">
      <View style={styles.panelHeader}>
        {compact ? (
          <Pressable
            onPress={close}
            accessibilityRole="button"
            accessibilityLabel={w.back}
            style={styles.iconButton}
            testID="activity-back"
          >
            <ThemedBack size={20} uniProps={foregroundColor} />
          </Pressable>
        ) : null}
        <Text style={styles.panelTitle}>{w.title}</Text>
        {compact ? null : (
          <Pressable
            onPress={close}
            accessibilityRole="button"
            accessibilityLabel={w.close}
            style={styles.iconButton}
            testID="activity-close"
          >
            <ThemedX size={16} uniProps={mutedColor} />
          </Pressable>
        )}
      </View>
      <ScrollView style={styles.panelBody} contentContainerStyle={styles.panelContent}>
        {tasks.length === 0 ? <Text style={styles.none}>{w.none}</Text> : null}
        {tasks.map((task) => (
          <TaskSection
            key={task.taskId}
            task={task}
            now={now}
            locale={locale}
            canStop={!readOnly}
            stopping={stopping.has(task.taskId)}
            onStop={handleStop}
          />
        ))}
        {tasks.length > 0 ? <Text style={styles.footer}>{w.footer}</Text> : null}
      </ScrollView>
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  chip: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
    paddingHorizontal: theme.spacing[2],
    paddingVertical: theme.spacing[0.5],
    borderRadius: theme.borderRadius.full,
    borderWidth: 1,
    borderColor: theme.colors.border,
    backgroundColor: theme.colors.surface1,
  },
  chipOpen: {
    backgroundColor: theme.colors.surface3,
  },
  chipText: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  panelDock: {
    width: 340,
    flexShrink: 0,
    borderLeftWidth: 1,
    borderLeftColor: theme.colors.border,
    backgroundColor: theme.colors.surface0,
    minHeight: 0,
  },
  panelPhone: {
    position: "absolute",
    top: 0,
    left: 0,
    right: 0,
    bottom: 0,
    zIndex: 50,
    backgroundColor: theme.colors.surface0,
  },
  panelHeader: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingHorizontal: theme.spacing[3],
    paddingVertical: theme.spacing[3],
    borderBottomWidth: 1,
    borderBottomColor: theme.colors.border,
  },
  panelTitle: {
    flex: 1,
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.semibold,
  },
  iconButton: {
    padding: theme.spacing[1],
  },
  panelBody: {
    flex: 1,
  },
  panelContent: {
    paddingBottom: theme.spacing[6],
  },
  section: {
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[4],
    borderBottomWidth: 1,
    borderBottomColor: theme.colors.border,
    gap: theme.spacing[1],
  },
  titleRow: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: theme.spacing[2],
  },
  title: {
    flex: 1,
    minWidth: 0,
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.semibold,
  },
  tag: {
    color: theme.colors.foregroundMuted,
    fontSize: 11,
    paddingHorizontal: theme.spacing[1.5],
    paddingVertical: 1,
    borderRadius: theme.borderRadius.base,
    backgroundColor: theme.colors.surface2,
    overflow: "hidden",
  },
  stop: {
    paddingHorizontal: theme.spacing[2],
    paddingVertical: 1,
    borderRadius: theme.borderRadius.base,
    borderWidth: 1,
    borderColor: theme.colors.border,
  },
  stopText: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  elapsedRow: {
    flexDirection: "row",
    alignItems: "baseline",
    gap: theme.spacing[2],
    marginBottom: theme.spacing[1],
  },
  elapsedLabel: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  elapsed: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.xl,
    fontWeight: theme.fontWeight.semibold,
    fontVariant: ["tabular-nums"],
  },
  elapsedDone: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    fontVariant: ["tabular-nums"],
  },
  heading: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    marginTop: theme.spacing[2],
  },
  mono: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
    fontFamily: theme.fontFamily.mono,
    backgroundColor: theme.colors.surface2,
    borderRadius: theme.borderRadius.lg,
    paddingHorizontal: theme.spacing[3],
    paddingVertical: theme.spacing[2],
  },
  result: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
    lineHeight: 19,
  },
  fact: {
    flexDirection: "row",
    justifyContent: "space-between",
    gap: theme.spacing[3],
    marginTop: theme.spacing[1],
  },
  factLabel: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  factValue: {
    flexShrink: 1,
    textAlign: "right",
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  none: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    paddingHorizontal: theme.spacing[4],
    paddingTop: theme.spacing[4],
  },
  footer: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    paddingHorizontal: theme.spacing[4],
    paddingTop: theme.spacing[4],
  },
}));

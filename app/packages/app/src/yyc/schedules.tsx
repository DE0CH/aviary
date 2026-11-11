/**
 * Schedules (Deyao, 2026-11-06; design: the approved mocks): YYC AI arms a message to be posted into this conversation
 * once or on a repeating rule, through its own REST API (the router's jarvis/schedules.mjs on the old stack, magpie's
 * on the new one). What staff see, all without buttons (changes go through the chat):
 *   - the AI's API call as a card in the chat: 「已設定排程：每個工作日 08:30」 and the next time (ScheduleToolCard);
 *   - a fired message (`<scheduled-message rule="…">…</scheduled-message>`) as a 排程訊息 bubble under a divider
 *     「排程 · 時間」 (ScheduledMessage);
 *   - a clock chip in the conversation's header, 「n 個排程」 (ScheduleChip), opening a panel beside the chat that
 *     pushes it narrower (full screen with a back arrow on a phone; SchedulesPanel): each schedule open, its rule and
 *     重複/單次, the next time large, the message, the conditions (時區, 結束, 設定) and the last runs, then the grey
 *     line 「在對話中告訴 YYC AI 即可修改或取消。」.
 * The panel reads GET /api/jarvis/sessions/<id>/schedules (the same shape on both stacks). Staff text is Chinese for
 * everyone (staff pages are in Chinese, CLAUDE.md "Staff preference").
 */
import React, { useEffect } from "react";
import { Pressable, ScrollView, Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { ChevronLeft, Clock, X } from "lucide-react-native";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { create } from "zustand";
import { api } from "@/yyc/api";
import { useFirstName } from "@/yyc/me";
import type { Theme } from "@/styles/theme";

import {
  nextLabel,
  OUTCOME_WORDS,
  type Schedule,
  shortLabel,
  WEEKDAY,
  ZONE_WORDS,
  type readScheduleCall,
} from "@/yyc/schedules-text";

export { parseScheduledMessage, readScheduleCall } from "@/yyc/schedules-text";
export type { Schedule, ScheduleRun } from "@/yyc/schedules-text";

// ---- data ----

const queryKey = (sessionId: string) => ["yyc-schedules", sessionId] as const;

/** The conversation's schedules (the panel's list), kept fresh while shown. */
export function useConversationSchedules(sessionId: string | null) {
  return useQuery({
    queryKey: queryKey(sessionId ?? ""),
    enabled: !!sessionId,
    queryFn: async () => (await api<{ schedules?: Schedule[] }>(`/api/jarvis/sessions/${encodeURIComponent(sessionId ?? "")}/schedules`)).schedules ?? [],
    refetchInterval: 30_000,
    staleTime: 5_000,
  });
}

/** Which conversation's panel is open. */
export const useSchedulesPanel = create<{ openFor: string | null; toggle: (sessionId: string) => void; close: () => void }>((set) => ({
  openFor: null,
  toggle: (sessionId) => set((s) => ({ openFor: s.openFor === sessionId ? null : sessionId })),
  close: () => set({ openFor: null }),
}));

const isLive = (s: Schedule) => s.state === "active" || s.state === "paused";

// ---- the chip ----

const ThemedClock = withUnistyles(Clock);
const ThemedX = withUnistyles(X);
const ThemedBack = withUnistyles(ChevronLeft);
const mutedColor = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const foregroundColor = (theme: Theme) => ({ color: theme.colors.foreground });
// the 排程訊息 blue: 600 on light; on dark the lighter 300 (600 is too dark to read there), as the app's dark info colour
const scheduleBlue = (theme: Theme) => (theme.colorScheme === "dark" ? theme.colors.palette.blue[300] : theme.colors.palette.blue[600]);
const accentColor = (theme: Theme) => ({ color: scheduleBlue(theme) });

/** 「n 個排程」 in the conversation's header (nothing when it has none); opens the panel. */
export function ScheduleChip({ sessionId }: { sessionId: string | null }) {
  const { data } = useConversationSchedules(sessionId);
  const open = useSchedulesPanel((s) => (sessionId ? s.openFor === sessionId : false));
  const toggle = useSchedulesPanel((s) => s.toggle);
  const all = data ?? [];
  const count = all.filter(isLive).length;
  // a one-off that fired stays in the panel for a day (its run): the chip stays too, without a count
  if (!sessionId || all.length === 0) return null;
  const label = count > 0 ? `${count} 個排程` : "排程";
  return (
    <Pressable
      testID="schedules-chip"
      accessibilityRole="button"
      accessibilityState={{ expanded: open }}
      accessibilityLabel={label}
      onPress={() => toggle(sessionId)}
      style={[styles.chip, open ? styles.chipOpen : null]}
    >
      <ThemedClock size={13} uniProps={mutedColor} />
      <Text style={styles.chipText}>{label}</Text>
    </Pressable>
  );
}

// ---- the panel ----

function Condition({ label, value }: { label: string; value: string }) {
  return (
    <View style={styles.condition}>
      <Text style={styles.conditionLabel}>{label}</Text>
      <Text style={styles.conditionValue}>{value}</Text>
    </View>
  );
}

function ScheduleSection({ s, name }: { s: Schedule; name: string | null }) {
  const repeating = s.kind === "cron";
  const tag = s.state === "paused" ? "已暫停" : s.state === "done" ? "已完成" : repeating ? "重複" : "單次";
  const runs = s.runs ?? [];
  return (
    <View style={styles.section} testID="schedule-section">
      <View style={styles.titleRow}>
        <Text style={styles.rule}>{repeating ? s.describe : "一次"}</Text>
        <Text style={styles.tag}>{tag}</Text>
      </View>
      <View style={styles.nextRow}>
        <Text style={styles.nextLabel}>下次</Text>
        <Text style={styles.next}>{s.state === "active" ? nextLabel(s.next, s.tz) : "沒有"}</Text>
      </View>
      <Text style={styles.heading}>訊息</Text>
      <Text style={styles.prompt}>{s.prompt}</Text>
      <Text style={styles.heading}>條件</Text>
      <Condition label="時區" value={ZONE_WORDS[s.tz] ?? s.tz} />
      {repeating ? <Condition label="結束" value={s.until ? shortLabel(s.until) : "沒有"} /> : null}
      <Condition label="設定" value={`${name ?? s.createdBy}，${shortLabel(s.createdAt)}`} />
      <Text style={styles.heading}>最近執行</Text>
      {runs.length === 0 ? <Text style={styles.runNone}>未執行過</Text> : null}
      {runs.map((r) => (
        <View key={`${r.due}`} style={styles.condition}>
          <Text style={styles.runDue}>{shortLabel(r.due)}</Text>
          <Text style={styles.runOutcome}>{OUTCOME_WORDS[r.outcome] ?? r.outcome}</Text>
        </View>
      ))}
    </View>
  );
}

/**
 * The panel beside the chat (it takes its width from the chat column, which gets narrower); on a phone it covers the
 * chat, with a back arrow.
 */
export function SchedulesPanel({ sessionId, compact }: { sessionId: string | null; compact: boolean }) {
  const open = useSchedulesPanel((s) => (sessionId ? s.openFor === sessionId : false));
  const close = useSchedulesPanel((s) => s.close);
  const { data } = useConversationSchedules(open ? sessionId : null);
  const name = useFirstName();
  useEffect(() => {
    // a conversation left with its panel open: closed, so the next one starts without it
    return () => {
      if (useSchedulesPanel.getState().openFor === sessionId) useSchedulesPanel.getState().close();
    };
  }, [sessionId]);
  if (!open || !sessionId) return null;
  const schedules = data ?? [];
  return (
    <View style={compact ? styles.panelPhone : styles.panelDock} testID="schedules-panel">
      <View style={styles.panelHeader}>
        {compact ? (
          <Pressable onPress={close} accessibilityRole="button" accessibilityLabel="返回" style={styles.iconButton} testID="schedules-back">
            <ThemedBack size={20} uniProps={foregroundColor} />
          </Pressable>
        ) : null}
        <Text style={styles.panelTitle}>排程</Text>
        {compact ? null : (
          <Pressable onPress={close} accessibilityRole="button" accessibilityLabel="關閉" style={styles.iconButton} testID="schedules-close">
            <ThemedX size={16} uniProps={mutedColor} />
          </Pressable>
        )}
      </View>
      <ScrollView style={styles.panelBody} contentContainerStyle={styles.panelContent}>
        {schedules.length === 0 ? <Text style={styles.runNone}>這個對話沒有排程。</Text> : null}
        {schedules.map((s) => (
          <ScheduleSection key={s.id} s={s} name={name ?? null} />
        ))}
        <Text style={styles.footer}>在對話中告訴 YYC AI 即可修改或取消。</Text>
      </ScrollView>
    </View>
  );
}

// ---- in the chat ----

/** The AI's schedules API call, as a card (no buttons): what was set and when it fires next. */
export function ScheduleToolCard({
  sessionId,
  call,
}: {
  sessionId: string | null;
  call: NonNullable<ReturnType<typeof readScheduleCall>>;
}) {
  const queryClient = useQueryClient();
  useEffect(() => {
    // the chip and the panel follow at once
    if (sessionId) void queryClient.invalidateQueries({ queryKey: queryKey(sessionId) });
  }, [call.action, call.schedule?.id, call.schedule?.state, queryClient, sessionId]);
  const s = call.schedule;
  const title =
    call.action === "deleted"
      ? "已取消排程"
      : call.action === "changed"
        ? s?.state === "paused"
          ? `已暫停排程：${s.describe}`
          : `已更改排程：${s?.describe ?? ""}`
        : `已設定排程：${s?.describe ?? ""}`;
  const sub = s && s.state === "active" ? `在這個對話 · 下次：${nextLabel(s.next, s.tz)}` : s ? "在這個對話" : null;
  return (
    <View style={styles.card} testID="schedule-card">
      <ThemedClock size={16} uniProps={foregroundColor} />
      <View style={styles.cardText}>
        <Text style={styles.cardTitle}>{title}</Text>
        {sub ? <Text style={styles.cardSub}>{sub}</Text> : null}
      </View>
    </View>
  );
}

/** A fired schedule's message: a divider 「排程 · 時間」 and the message marked 排程訊息 (not typed by the person). */
export function ScheduledMessage({ prompt, timestamp }: { rule: string; prompt: string; timestamp: number }) {
  const when = new Date(timestamp);
  const label = Number.isNaN(when.getTime())
    ? ""
    : `${when.getMonth() + 1}月${when.getDate()}日（星期${WEEKDAY[when.getDay()]}）${String(when.getHours()).padStart(2, "0")}:${String(when.getMinutes()).padStart(2, "0")}`;
  return (
    <View style={styles.firedWrap} testID="scheduled-message">
      <View style={styles.divider}>
        <View style={styles.dividerLine} />
        <Text style={styles.dividerText}>排程 · {label}</Text>
        <View style={styles.dividerLine} />
      </View>
      <View style={styles.firedRow}>
        <View style={styles.firedBubble}>
          <View style={styles.firedLabelRow}>
            <ThemedClock size={12} uniProps={accentColor} />
            <Text style={styles.firedLabel}>排程訊息</Text>
          </View>
          <Text style={styles.firedText} selectable>
            {prompt}
          </Text>
        </View>
      </View>
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
    alignItems: "center",
    gap: theme.spacing[2],
  },
  rule: {
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
  nextRow: {
    flexDirection: "row",
    alignItems: "baseline",
    gap: theme.spacing[2],
    marginBottom: theme.spacing[2],
  },
  nextLabel: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  next: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.xl,
    fontWeight: theme.fontWeight.semibold,
  },
  heading: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    marginTop: theme.spacing[2],
  },
  prompt: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    lineHeight: 20,
    backgroundColor: theme.colors.surface2,
    borderRadius: theme.borderRadius.lg,
    paddingHorizontal: theme.spacing[3],
    paddingVertical: theme.spacing[2],
  },
  condition: {
    flexDirection: "row",
    justifyContent: "space-between",
    gap: theme.spacing[3],
  },
  conditionLabel: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  conditionValue: {
    flexShrink: 1,
    textAlign: "right",
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  runDue: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  runOutcome: {
    flexShrink: 1,
    textAlign: "right",
    color: theme.colors.foreground,
    fontSize: theme.fontSize.sm,
  },
  runNone: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    paddingHorizontal: theme.spacing[4],
    paddingTop: theme.spacing[1],
  },
  footer: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    paddingHorizontal: theme.spacing[4],
    paddingTop: theme.spacing[4],
  },
  card: {
    flexDirection: "row",
    alignItems: "flex-start",
    gap: theme.spacing[3],
    borderWidth: 1,
    borderColor: theme.colors.border,
    borderRadius: theme.borderRadius.xl,
    backgroundColor: theme.colors.surface1,
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[3],
    marginVertical: theme.spacing[2],
  },
  cardText: {
    flex: 1,
    minWidth: 0,
    gap: theme.spacing[0.5],
  },
  cardTitle: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.semibold,
  },
  cardSub: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  firedWrap: {
    marginTop: theme.spacing[4],
    marginBottom: theme.spacing[4],
    gap: theme.spacing[3],
  },
  divider: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
  },
  dividerLine: {
    flex: 1,
    height: 1,
    backgroundColor: theme.colors.border,
  },
  dividerText: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  firedRow: {
    flexDirection: "row",
    justifyContent: "flex-end",
  },
  firedBubble: {
    maxWidth: "100%",
    flexShrink: 1,
    // dark: the person's own bubble surface (components/message.tsx) with a blue edge, not a saturated blue box
    backgroundColor: theme.colorScheme === "dark" ? theme.colors.surface3 : theme.colors.palette.blue[50],
    borderWidth: 1,
    borderColor: theme.colorScheme === "dark" ? "rgba(147,197,253,0.45)" : theme.colors.palette.blue[200],
    borderRadius: theme.borderRadius["2xl"],
    borderTopRightRadius: theme.borderRadius.sm,
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[3],
    gap: theme.spacing[1],
  },
  firedLabelRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
  },
  firedLabel: {
    color: scheduleBlue(theme),
    fontSize: theme.fontSize.sm,
    fontWeight: theme.fontWeight.medium,
  },
  firedText: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.content,
    lineHeight: Math.round(theme.fontSize.content * 1.4),
  },
}));

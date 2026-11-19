/**
 * The question and wait cards (Deyao, 2026-11-18; design: the approved three-state mock). A question the AI asked
 * with `ask`, or a command's wait on the person for something outside the chat (Jarvis 4's Inbox), is one card:
 *   - waiting on the person: yellow, where it was asked (a question's options are in the answer card at the bottom);
 *   - answered, not read by the AI yet: where it was asked, the answer filled in, a hollow mark and "Not read yet";
 *   - read: on the tool call whose result first carried the answer into the AI's context, its header saying when it
 *     was asked when that was a while before.
 * The readings are yyc/person-cards-text.ts; where each shows is the server's (aviary magpie rule 11).
 */
import React from "react";
import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { CircleHelp, Inbox } from "lucide-react-native";
import type { Theme } from "@/styles/theme";
import {
  cardAnswers,
  cardQuestions,
  cardWords,
  clock,
  moved,
  stateWord,
  waitText,
  type CardLocale,
  type CardQuestion,
  type CardView,
  type CardAnswer,
  type PersonCard,
} from "@/yyc/person-cards-text";

const ThemedHelp = withUnistyles(CircleHelp);
const ThemedInbox = withUnistyles(Inbox);
const mutedColor = (theme: Theme) => ({ color: theme.colors.foregroundMuted });
const warnColor = (theme: Theme) => ({ color: theme.colors.statusDotWarning });

function useCardLocale(): CardLocale {
  const { i18n } = useTranslation();
  return (i18n.language ?? "").toLowerCase().startsWith("zh") ? "zh" : "en";
}

/** One card, in the given state. */
export function PersonCardView({ card, view }: { card: PersonCard; view: CardView }) {
  const locale = useCardLocale();
  const w = cardWords(locale);
  const waiting = view === "waiting";
  const unread = view === "unread";
  const isWait = card.kind === "wait";
  const wait = isWait ? waitText(card) : null;
  const label = isWait ? (wait?.label ?? w.wait) : w.question;
  const header = view === "read" && moved(card) ? w.asked(label, clock(card.askedAt)) : label;
  const settled = `${stateWord(card, locale)} ${clock(card.answeredAt)}`.trim();
  const Icon = isWait ? ThemedInbox : ThemedHelp;
  const answers = cardAnswers(card);
  return (
    <View
      style={[styles.card, waiting && styles.cardWaiting]}
      testID={`person-card-${view}`}
      accessibilityLabel={`${label}: ${waiting ? w.waiting : unread ? w.unread : settled}`}
    >
      <View style={styles.head}>
        <Icon size={16} uniProps={waiting ? warnColor : mutedColor} />
        <Text style={styles.headLabel} numberOfLines={1}>
          {header}
        </Text>
        <View style={styles.tag}>
          {waiting ? <View style={styles.dot} /> : unread ? <View style={styles.ring} /> : null}
          <Text style={waiting || unread ? styles.tagStrong : styles.tagText}>
            {waiting ? w.waiting : unread ? w.unread : settled}
          </Text>
        </View>
      </View>
      {wait ? (
        <>
          <Text style={styles.title}>{wait.title}</Text>
          {wait.why ? <Text style={styles.small}>{wait.why}</Text> : null}
          {!waiting && w.outcome[card.state] ? <Text style={styles.outcome}>{w.outcome[card.state]}</Text> : null}
        </>
      ) : (
        cardQuestions(card).map((q) => (
          <QuestionBlock key={q.question} q={q} answer={answers.get(q.question)} showOptions={!waiting} />
        ))
      )}
      {!isWait && card.state === "declined" ? <Text style={styles.small}>{w.declined}</Text> : null}
      {waiting && !isWait ? <Text style={styles.small}>{w.answerBelow}</Text> : null}
      {unread ? <Text style={styles.small}>{w.unreadNote(stateWord(card, locale), clock(card.answeredAt))}</Text> : null}
    </View>
  );
}

function QuestionBlock({ q, answer, showOptions }: { q: CardQuestion; answer?: CardAnswer; showOptions: boolean }) {
  const locale = useCardLocale();
  const picked = new Set(answer?.picked ?? []);
  const mark = q.multiSelect ? styles.markSquare : styles.markRound;
  return (
    <View style={styles.question}>
      <Text style={styles.title}>{q.question}</Text>
      {showOptions
        ? q.options.map((o) => {
            const on = picked.has(o.label);
            return (
              <View key={o.label} style={styles.option}>
                <View style={[mark, on && styles.markOn]} />
                <View style={styles.optionText}>
                  <Text style={on ? styles.optionOn : styles.optionOff}>{o.label}</Text>
                  {o.description ? <Text style={styles.small}>{o.description}</Text> : null}
                </View>
              </View>
            );
          })
        : null}
      {showOptions && answer?.other ? (
        <View style={styles.option}>
          <View style={[mark, styles.markOn]} />
          <Text style={styles.optionOn}>{answer.other}</Text>
        </View>
      ) : null}
      {answer?.notes ? <Text style={styles.small}>{cardWords(locale).note(answer.notes)}</Text> : null}
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  card: {
    marginVertical: theme.spacing[3],
    borderWidth: 1,
    borderColor: theme.colors.border,
    borderRadius: theme.borderRadius.xl,
    backgroundColor: theme.colors.surface1,
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[3],
    gap: theme.spacing[3],
  },
  cardWaiting: { borderColor: theme.colors.statusDotWarning },
  head: { flexDirection: "row", alignItems: "center", gap: theme.spacing[2] },
  headLabel: { flex: 1, minWidth: 0, color: theme.colors.foregroundMuted, fontSize: theme.fontSize.sm },
  tag: { flexDirection: "row", alignItems: "center", gap: theme.spacing[1], flexShrink: 0 },
  dot: { width: 8, height: 8, borderRadius: 999, backgroundColor: theme.colors.statusDotWarning },
  ring: { width: 8, height: 8, borderRadius: 999, borderWidth: 1.5, borderColor: theme.colors.foregroundMuted },
  tagText: { color: theme.colors.foregroundMuted, fontSize: theme.fontSize.sm },
  tagStrong: { color: theme.colors.foreground, fontSize: theme.fontSize.sm, fontWeight: theme.fontWeight.semibold },
  question: { gap: theme.spacing[2] },
  title: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.semibold,
    lineHeight: 22,
  },
  option: { flexDirection: "row", alignItems: "flex-start", gap: theme.spacing[2] },
  optionText: { flex: 1, gap: 2 },
  markRound: {
    width: 14,
    height: 14,
    marginTop: 4,
    borderRadius: 999,
    borderWidth: 1,
    borderColor: theme.colors.foregroundMuted,
  },
  markSquare: {
    width: 14,
    height: 14,
    marginTop: 4,
    borderRadius: 3,
    borderWidth: 1,
    borderColor: theme.colors.foregroundMuted,
  },
  markOn: { backgroundColor: theme.colors.foreground, borderColor: theme.colors.foreground },
  optionOff: { color: theme.colors.foregroundMuted, fontSize: theme.fontSize.base, lineHeight: 22 },
  optionOn: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    lineHeight: 22,
    fontWeight: theme.fontWeight.semibold,
  },
  outcome: { color: theme.colors.foreground, fontSize: theme.fontSize.base, fontWeight: theme.fontWeight.semibold },
  small: { color: theme.colors.foregroundMuted, fontSize: theme.fontSize.sm, lineHeight: 18 },
}));

/**
 * The readings of the question and wait cards (yyc/person-cards.tsx; aviary magpie rule 11, Deyao 2026-11-18): a
 * question the AI asked with `ask`, or a command's wait on the person for something outside the chat (Jarvis 4's
 * Inbox), shown as one card in three states:
 *   - waiting on the person, where it was asked;
 *   - answered, not read by the AI yet, still where it was asked;
 *   - read, on the tool call whose result first carried the answer into the AI's context.
 * The server puts the card on the call where it was asked (`metadata.personAsk`) and on the call that read the answer
 * (`metadata.personRead`); the card where it was asked shows nothing once it has been read.
 * No React Native here, so the unit tests run without it.
 */

/** A card as the server sends it (magpie-api `PersonCard`). */
export interface PersonCard {
  kind: "question" | "wait" | string;
  id: string;
  input: unknown;
  marker: string;
  /** `open`, `answered`, `declined`, or a wait's outcome (`approved`, `denied`, `lapsed`, …). */
  state: string;
  answer?: unknown;
  askedAt: string;
  answeredAt?: string | null;
  readBy?: string | null;
}

export type CardView = "waiting" | "unread" | "read";

/**
 * The cards are switched off (Deyao, 2026-11-19: a question showed twice, "not read yet" was wrong for every session
 * on an older image, and scrolling misbehaved). Off, the app is as before them: a card's own row is left out of the
 * conversation and a call that read an answer is an ordinary step. They come back once they work on the whole stack.
 */
export const PERSON_CARDS = false;

/** A card's own row while the cards are off: left out of the conversation altogether. */
export function isHiddenPersonRow(metadata: unknown): boolean {
  return !PERSON_CARDS && readPersonAsk(metadata) !== null;
}

function isCard(v: unknown): v is PersonCard {
  const c = v as Partial<PersonCard> | null;
  return !!c && typeof c.id === "string" && typeof c.state === "string" && typeof c.kind === "string";
}

/** The card of the call where it was asked, if this call is one. */
export function readPersonAsk(metadata: unknown): PersonCard | null {
  const c = (metadata as { personAsk?: unknown } | null | undefined)?.personAsk;
  return isCard(c) ? c : null;
}

/** The cards whose answers this call's result carried into the AI's context first. */
export function readPersonRead(metadata: unknown): PersonCard[] {
  const r = (metadata as { personRead?: unknown } | null | undefined)?.personRead;
  return Array.isArray(r) ? r.filter(isCard) : [];
}

/**
 * Where it was asked: waiting while open, unread once settled, and nothing once read (the card shows on the call that
 * read it). Where it was read: read.
 */
export function cardViewWhereAsked(card: PersonCard): CardView | null {
  if (card.readBy) return null;
  return card.state === "open" ? "waiting" : "unread";
}

/** Whether a call carries a card in either place: it is never folded into a group or a turn's summary. */
export function isPersonCardCall(metadata: unknown): boolean {
  if (!PERSON_CARDS) return false;
  return readPersonAsk(metadata) !== null || readPersonRead(metadata).length > 0;
}

// ---- a question's content ----

export interface CardQuestion {
  question: string;
  header?: string;
  multiSelect: boolean;
  options: { label: string; description?: string }[];
}

export function cardQuestions(card: PersonCard): CardQuestion[] {
  const qs = (card.input as { questions?: unknown } | null)?.questions;
  if (!Array.isArray(qs)) return [];
  return qs
    .filter((q): q is Record<string, unknown> => !!q && typeof (q as { question?: unknown }).question === "string")
    .map((q) => ({
      question: q.question as string,
      header: typeof q.header === "string" ? q.header : undefined,
      multiSelect: q.multiSelect === true,
      options: Array.isArray(q.options)
        ? (q.options as Record<string, unknown>[])
            .filter((o) => o && typeof o.label === "string")
            .map((o) => ({
              label: o.label as string,
              description: typeof o.description === "string" ? o.description : undefined,
            }))
        : [],
    }));
}

/** One question's answer: the options picked, what was typed instead, and the note. */
export interface CardAnswer {
  picked: string[];
  other: string | null;
  notes: string | null;
}

/**
 * The person's answer to each question, from the resolution (`updatedInput.answers[question]`, a label, labels joined
 * by ", " for several, or free text; `updatedInput.annotations[question].notes`).
 */
export function cardAnswers(card: PersonCard): Map<string, CardAnswer> {
  const out = new Map<string, CardAnswer>();
  const input = (card.answer as { updatedInput?: unknown } | null)?.updatedInput as
    | { answers?: Record<string, unknown>; annotations?: Record<string, { notes?: unknown }> }
    | undefined;
  for (const q of cardQuestions(card)) {
    const raw = input?.answers?.[q.question];
    const notes = input?.annotations?.[q.question]?.notes;
    if (raw === undefined && typeof notes !== "string") continue;
    const text = typeof raw === "string" ? raw : raw === undefined ? "" : JSON.stringify(raw);
    const labels = new Set(q.options.map((o) => o.label));
    const parts = text ? (q.multiSelect ? text.split(/,\s*/) : [text]) : [];
    const picked = parts.filter((p) => labels.has(p));
    const rest = parts.filter((p) => !labels.has(p)).join(", ");
    out.set(q.question, {
      picked,
      other: rest || null,
      notes: typeof notes === "string" && notes.trim() ? notes.trim() : null,
    });
  }
  return out;
}

// ---- a wait's content ----

export function waitText(card: PersonCard): { label: string | null; title: string; why: string } {
  const i = (card.input ?? {}) as { label?: unknown; title?: unknown; why?: unknown };
  const s = (v: unknown) => (typeof v === "string" ? v.trim() : "");
  return { label: s(i.label) || null, title: s(i.title), why: s(i.why) };
}

// ---- words ----

export type CardLocale = "zh" | "en";

const WORDS = {
  en: {
    question: "Question for you",
    wait: "Request",
    waiting: "Waiting for you",
    unread: "Not read yet",
    answerBelow: "Answer it in the card at the bottom.",
    unreadNote: (verb: string, at: string) => `${verb} ${at}. The AI hasn't read it yet.`,
    asked: (label: string, at: string) => `${label}, asked ${at}`,
    note: (n: string) => `Your note: ${n}`,
    declined: "You declined.",
    states: {
      answered: "Answered",
      declined: "Declined",
      approved: "Approved",
      denied: "Denied",
      lapsed: "Lapsed",
      dropped: "Dropped",
      failed: "Failed",
    } as Record<string, string>,
    outcome: { approved: "You approved it", denied: "You denied it", lapsed: "Nobody decided it in time" } as Record<
      string,
      string
    >,
  },
  zh: {
    question: "想問你",
    wait: "請求",
    waiting: "等你回覆",
    unread: "未讀",
    answerBelow: "喺下面嘅卡片回答。",
    unreadNote: (verb: string, at: string) => `${at} ${verb}。AI 仲未讀到。`,
    asked: (label: string, at: string) => `${label}（${at} 問）`,
    note: (n: string) => `你嘅備註：${n}`,
    declined: "你拒絕咗。",
    states: {
      answered: "已回答",
      declined: "已拒絕",
      approved: "已批准",
      denied: "已拒絕",
      lapsed: "已過期",
      dropped: "已撤回",
      failed: "失敗",
    } as Record<string, string>,
    outcome: { approved: "你批准咗", denied: "你拒絕咗", lapsed: "冇人及時決定" } as Record<string, string>,
  },
};

export function cardWords(locale: CardLocale) {
  return WORDS[locale];
}

/** A settled card's state in a word ("Answered", "Approved"); an unknown outcome as the server wrote it. */
export function stateWord(card: PersonCard, locale: CardLocale): string {
  return WORDS[locale].states[card.state] ?? card.state;
}

/** "14:15" in the device's time. */
export function clock(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return `${d.getHours()}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/** Whether the card moved: it is shown where it was read, a while after it was asked (its header says when). */
export function moved(card: PersonCard): boolean {
  if (!card.answeredAt) return false;
  return Date.parse(card.answeredAt) - Date.parse(card.askedAt) > 5 * 60_000;
}

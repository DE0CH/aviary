// The empty new-conversation screen's greeting (Deyao, 2026-10-21: playful, by time of day, like claude.ai's).
// The phrase set is the one Deyao approved on 2026-10-21:
//   en:    the English phrases come from Wiktionary, "Category:English greetings" (CC BY-SA)
//          https://en.wiktionary.org/wiki/Category:English_greetings
//   zh-HK: the Cantonese versions were translated by Claude in the session and approved by Deyao (2026-10-21).
// Each locale picks only from its own lists; the two locales' lists run in parallel (same slots, same order).
// Same as i18n.ts Locale (not imported: the server tests load this file without the DOM types).
type Locale = "zh-HK" | "en";

export type Slot = "morning" | "afternoon" | "evening" | "night";

interface Phrases {
  /** Only in that time slot. */
  slots: Record<Slot, readonly string[]>;
  /** In every slot's pool. */
  anytime: readonly string[];
  /** In the morning and afternoon pools. */
  daytime: readonly string[];
  /** Only for a user who has earlier conversations. */
  returning: readonly string[];
}

export const GREETINGS: Record<Locale, Phrases> = {
  "zh-HK": {
    slots: {
      morning: ["早晨", "早呀", "早晨！今日精神嗎？"],
      afternoon: ["下晝好", "食咗飯未呀？"],
      evening: ["夜晚好", "食咗飯未呀？"],
      night: ["夜喇，仲未瞓？", "早唞"],
    },
    anytime: [
      "你好呀", "你好", "哈囉", "嗨", "喂，你好", "哈囉哈囉", "嘿", "你好嗎？", "點呀？", "近排點呀？", "一切順利嗎？",
      "最近過成點？", "有咩新嘢？", "而家發生緊咩事？",
    ],
    daytime: ["今日打得掂嗎？"],
    returning: ["又見面喇", "歡迎返嚟"],
  },
  en: {
    slots: {
      morning: ["Good morning", "Morning", "Top of the morning"],
      afternoon: ["Good afternoon", "How's it going?"],
      evening: ["Good evening", "How's everything?"],
      night: ["Hello again", "Hi there"],
    },
    anytime: [
      "Good day", "Hello", "Hi", "Hey", "Hello there", "Hiya", "Hey there", "How are you?", "How are you doing?",
      "How's it going?", "How's everything?", "How's life?", "What's new?", "What's going on?",
    ],
    daytime: ["How goes the battle?"],
    returning: ["Hello again", "Welcome"],
  },
};

/** 05:00–11:59 morning, 12:00–17:59 afternoon, 18:00–22:59 evening, 23:00–04:59 late night, Hong Kong time. */
export function slotAt(d: Date): Slot {
  const h = Number(new Intl.DateTimeFormat("en-GB", { hour: "2-digit", hourCycle: "h23", timeZone: "Asia/Hong_Kong" }).format(d));
  if (h >= 5 && h < 12) return "morning";
  if (h >= 12 && h < 18) return "afternoon";
  if (h >= 18 && h < 23) return "evening";
  return "night";
}

/** The phrases a slot draws from: its own, the any-time ones, the daytime ones (morning, afternoon) and, for a
 * user with earlier conversations, the returning ones. */
export function pool(loc: Locale, slot: Slot, returning = false): string[] {
  const p = GREETINGS[loc];
  return [
    ...p.slots[slot],
    ...p.anytime,
    ...(slot === "morning" || slot === "afternoon" ? p.daytime : []),
    ...(returning ? p.returning : []),
  ];
}

/** One phrase of the slot's pool (r in [0, 1): random per page load), with the first name if the identity gave one:
 * "Good morning, Deyao" / 「早晨，Deyao」; a question keeps its mark at the end (「點呀，Tai Man？」). */
export function greeting(loc: Locale, d: Date, r: number, name?: string | null, returning = false): string {
  const list = pool(loc, slotAt(d), returning);
  let g = list[Math.min(list.length - 1, Math.floor(r * list.length))]!;
  const n = name?.trim();
  if (!n) return g;
  const end = /[？?]$/.exec(g)?.[0] ?? "";
  if (end) g = g.slice(0, -1);
  return loc === "en" ? `${g}, ${n}${end}` : `${g}，${n}${end}`;
}

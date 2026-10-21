// The empty new-conversation screen's greeting (Deyao, 2026-10-21: playful, by time of day, like claude.ai's).
// The phrases are NOT made up here: each is taken from a published list (short common phrases; both sources are
// CC BY-SA, attribution below and in docs/CHAT.md):
//   zh-HK: Wikivoyage, "Cantonese phrasebook" (Basics) https://en.wikivoyage.org/wiki/Cantonese_phrasebook
//            早晨 (good morning), 你好 (hello), 食飽未？ (colloquial "how are you", lit. "have you eaten?"),
//            晚安 (good night), 早抖 (colloquial good night; written 早唞 in Hong Kong, Wiktionary
//            https://en.wiktionary.org/wiki/早唞 "variant form of 早敨": "good night; have a good night")
//          Wiktionary 午安 https://en.wiktionary.org/wiki/午安 ("good afternoon", Cantonese ng5 on1) and
//          晚上好 https://en.wiktionary.org/wiki/晚上好 ("good evening", Cantonese maan5 soeng6 hou2)
//   en:    Wiktionary, "Category:English greetings" https://en.wiktionary.org/wiki/Category:English_greetings
// Time slots (Hong Kong time) follow CantoneseClass101's "How do you greet someone at different times of the day in
// Cantonese" (早晨 in the morning, 午安 from noon to about 6 pm, then the evening ones).
// Same as i18n.ts Locale (not imported: the server tests load this file without the DOM types).
type Locale = "zh-HK" | "en";

export type Slot = "morning" | "afternoon" | "evening" | "night";

export const GREETINGS: Record<Locale, Record<Slot, readonly string[]>> = {
  "zh-HK": {
    morning: ["早晨", "你好"],
    afternoon: ["午安", "你好", "食飽未？"],
    evening: ["晚上好", "你好", "食飽未？"],
    night: ["早唞", "晚安"],
  },
  en: {
    morning: ["Good morning", "Morning", "Top of the morning"],
    afternoon: ["Good afternoon", "Hello there", "How's it going?"],
    evening: ["Good evening", "Hey there", "How's everything?"],
    night: ["Hello again", "Hi there", "Hey there"],
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

/** One of the slot's phrases (r in [0, 1): random per page load), with the first name if the identity gave one:
 * "Good morning, Deyao" / "早晨，Deyao"; a question keeps its mark at the end ("食飽未，Deyao？"). */
export function greeting(loc: Locale, d: Date, r: number, name?: string | null): string {
  const list = GREETINGS[loc][slotAt(d)];
  let g = list[Math.min(list.length - 1, Math.floor(r * list.length))]!;
  const n = name?.trim();
  if (!n) return g;
  const end = /[？?！!]$/.exec(g)?.[0] ?? "";
  if (end) g = g.slice(0, -1);
  return loc === "en" ? `${g}, ${n}${end}` : `${g}，${n}${end}`;
}

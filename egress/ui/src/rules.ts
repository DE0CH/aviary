// Small UI decisions with no DOM and no imports, so the server-side test suite (chat/test/ui-rules.test.ts) can check
// them directly.

/** The composer's model picker: only when the instance offers more than one model (one model = no choice to make). */
export const showModelPicker = (models: readonly unknown[]): boolean => models.length > 1;

/** A budget refusal's details from a turn's `error`/`turn_finished` event (code "budget"; docs/CHAT.md "Budget"). */
export function budgetOf(d: any): { limitUsd: number; ownerName: string | null } | undefined {
  return d?.code === "budget" && typeof d.limitUsd === "number" ? { limitUsd: d.limitUsd, ownerName: typeof d.ownerName === "string" ? d.ownerName : null } : undefined;
}

/** "30", "12.50": the limit as the budget message words it. */
export const usdAmount = (usd: number): string => (Number.isInteger(usd) ? String(usd) : usd.toFixed(2));

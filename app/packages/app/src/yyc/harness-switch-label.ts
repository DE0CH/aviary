import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { useHarnessSwitchStore } from "@/yyc/harness-switch-store";

/** The line beside the working indicator while a moved conversation's first reply sums up what came before. */
export function useSummarisingLabel(agentId: string, running: boolean): string | null {
  const { t } = useTranslation();
  const move = useHarnessSwitchStore((state) => state.moves[agentId] ?? null);
  const seen = useHarnessSwitchStore((state) => state.seen);
  useEffect(() => {
    if (move) seen(agentId, running);
  }, [agentId, move, running, seen]);
  return move && running ? t("conversation.model.move.summarising") : null;
}

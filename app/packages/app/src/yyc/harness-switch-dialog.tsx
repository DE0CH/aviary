import { useCallback, useMemo } from "react";
import { Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { AdaptiveModalSheet, type SheetHeader } from "@/components/adaptive-modal-sheet";
import { Button } from "@/components/ui/button";
import { useHarnessSwitchStore } from "@/yyc/harness-switch-store";

/**
 * Asked before a model pick moves a conversation to another harness (yyc/model-menu.tsx): the conversation so far
 * is summed up for the new model. Mounted once, in the hosts chunk (loading/hosts-chunk.tsx).
 */
export function HarnessSwitchDialogHost() {
  const { t } = useTranslation();
  const question = useHarnessSwitchStore((state) => state.question);
  const answer = useHarnessSwitchStore((state) => state.answer);
  const cancel = useCallback(() => answer(false), [answer]);
  const proceed = useCallback(() => answer(true), [answer]);
  const header = useMemo<SheetHeader>(
    () => ({ title: t("conversation.model.move.title", { model: question?.modelLabel ?? "" }) }),
    [question?.modelLabel, t],
  );
  if (!question) return null;
  return (
    <AdaptiveModalSheet visible onClose={cancel} header={header} testID="yyc-harness-switch">
      <View style={styles.body}>
        <Text style={styles.text}>{t("conversation.model.move.message")}</Text>
        <View style={styles.actions}>
          <Button
            variant="secondary"
            size="sm"
            style={styles.action}
            onPress={cancel}
            testID="yyc-harness-switch-cancel"
          >
            {t("common.actions.cancel")}
          </Button>
          <Button
            variant="default"
            size="sm"
            style={styles.action}
            onPress={proceed}
            testID="yyc-harness-switch-continue"
          >
            {t("conversation.model.move.continue")}
          </Button>
        </View>
      </View>
    </AdaptiveModalSheet>
  );
}

const styles = StyleSheet.create((theme) => ({
  body: {
    gap: theme.spacing[3],
    paddingBottom: theme.spacing[2],
  },
  text: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.base,
    lineHeight: theme.fontSize.base * 1.5,
  },
  actions: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
  },
  action: {
    flex: 1,
  },
}));

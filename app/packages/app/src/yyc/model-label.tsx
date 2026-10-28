import { useEffect, useState } from "react";
import { Text, View } from "react-native";
import { StyleSheet, withUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { Info } from "lucide-react-native";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { api } from "@/yyc/api";
import { isWeb } from "@/constants/platform";
import { ICON_SIZE } from "@/styles/theme";

interface ModelsResponse {
  data?: { models?: { OPTIONS?: Array<{ value?: unknown; label?: unknown }>; DEFAULT?: unknown } };
}

let labelPromise: Promise<string | null> | null = null;

/** The model staff chat with: config `models`' label for `defaultModel` (CloudCLI's /api/providers/claude/models). */
function loadModelLabel(): Promise<string | null> {
  labelPromise ??= api<ModelsResponse>("/api/providers/claude/models")
    .then((body) => {
      const models = body?.data?.models;
      const current = models?.OPTIONS?.find((option) => option.value === models.DEFAULT);
      return typeof current?.label === "string" ? current.label : null;
    })
    .catch(() => {
      labelPromise = null;
      return null;
    });
  return labelPromise;
}

const ThemedInfo = withUnistyles(Info);
const infoColor = (theme: { colors: { foregroundMuted: string } }) => ({ color: theme.colors.foregroundMuted });

/**
 * Where Paseo's composer shows its model picker: the model's name, read-only, and an (i) saying the admin sets it
 * (docs/paseo-ui.md). Hover opens it on the web, a tap on a phone.
 */
export function YycModelLabel() {
  const { t } = useTranslation();
  const [label, setLabel] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    void loadModelLabel().then((value) => {
      if (live) setLabel(value);
    });
    return () => {
      live = false;
    };
  }, []);

  if (!label) return null;
  return (
    <View style={styles.row} testID="yyc-model-label">
      <Text style={styles.label} numberOfLines={1}>
        {label}
      </Text>
      <Tooltip delayDuration={150} enabledOnDesktop enabledOnMobile>
        <TooltipTrigger accessibilityRole="button" accessibilityLabel={t("conversation.model.infoLabel")} hitSlop={8}>
          <ThemedInfo size={isWeb ? ICON_SIZE.sm : ICON_SIZE.md} uniProps={infoColor} />
        </TooltipTrigger>
        <TooltipContent side="top">
          <Text style={styles.tooltipText}>{t("conversation.model.info")}</Text>
        </TooltipContent>
      </Tooltip>
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  row: {
    // the composer's left group aligns its children to the bottom, which put this one line of text below the centre of
    // the attach button (+) beside it (Deyao, 2026-10-27): centred on the button instead
    alignSelf: "center",
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[1],
    paddingHorizontal: theme.spacing[2],
    // The composer's control size (composer/input/presentation.tsx CONTROL_SIZE).
    minHeight: 28,
    minWidth: 0,
    flexShrink: 1,
  },
  // The model chip it replaces (composer/agent-controls/control.tsx toolbarValue).
  label: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.normal,
    flexShrink: 1,
  },
  tooltipText: {
    color: theme.colors.popoverForeground,
    fontSize: theme.fontSize.sm,
    maxWidth: 260,
  },
}));

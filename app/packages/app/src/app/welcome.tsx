import { useCallback } from "react";
import { Image, Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { Button } from "@/components/ui/button";

const YYC_LOGO = require("../../assets/images/yyc-logo.png");

/**
 * Where the app lands when the server that served it cannot be reached (Paseo's own welcome screen offers to pair
 * other hosts; here there is only this one, docs/paseo-ui.md).
 */
export default function WelcomeRoute() {
  const { t } = useTranslation();
  const reload = useCallback(() => {
    if (typeof window !== "undefined") window.location.assign("/");
  }, []);
  return (
    <View style={styles.screen}>
      <Image source={YYC_LOGO} style={styles.logo} accessibilityIgnoresInvertColors />
      <Text style={styles.title}>{t("conversation.offline.title")}</Text>
      <Text style={styles.body}>{t("conversation.offline.body")}</Text>
      <Button variant="default" size="sm" onPress={reload}>
        {t("conversation.offline.retry")}
      </Button>
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  screen: {
    flex: 1,
    alignItems: "center",
    justifyContent: "center",
    gap: theme.spacing[3],
    padding: theme.spacing[6],
    backgroundColor: theme.colors.surface0,
  },
  logo: {
    width: 56,
    height: 56,
    borderRadius: 15,
    marginBottom: theme.spacing[2],
  },
  title: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.lg,
    fontWeight: theme.fontWeight.semibold,
  },
  body: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    textAlign: "center",
    marginBottom: theme.spacing[2],
  },
}));

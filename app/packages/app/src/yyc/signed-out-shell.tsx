import React, { useCallback } from "react";
import { Image, Pressable, Text, TextInput, View } from "react-native";
import { StyleSheet, useUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { LogIn, Plus, Search, Trash2 } from "lucide-react-native";
import { isWeb } from "@/constants/platform";
import { isCompactBreakpoint } from "@/constants/layout";

const YYC_LOGO = require("../../assets/images/yyc-logo.png");

/**
 * Whether the server marked this page signed out (web only): the router serves the built app to a visitor without an
 * Access token with <meta name="yyc-signed-out" content="1"> (DE0CH/claudecodeui jarvis/router.mjs, "Signed out").
 */
export function isSignedOutPage(): boolean {
  if (!isWeb || typeof document === "undefined") return false;
  return document.querySelector('meta[name="yyc-signed-out"]')?.getAttribute("content") === "1";
}

/** 「登入」: the sign-in path is behind Cloudflare Access, which signs the person in and sends them back here. */
function signIn() {
  if (typeof window === "undefined") return;
  const here = window.location.pathname + window.location.search;
  window.location.assign(`/api/jarvis/signin?next=${encodeURIComponent(here)}`);
}

/**
 * YYC AI's interface for a signed-out visitor (Deyao, 2026-10-27, "interface first"): the new-chat screen's layout
 * (sidebar on wide screens, the greeting, the composer) with the chat disabled and 「登入」 buttons. Nothing connects
 * to the server: no WebSocket, no API call, no user data, until the person has signed in.
 */
export function SignedOutShell() {
  const { t } = useTranslation();
  const { theme, rt } = useUnistyles();
  const compact = isCompactBreakpoint(rt.breakpoint);
  const muted = theme.colors.foregroundMuted;
  const onSignIn = useCallback(() => signIn(), []);

  const signInButton = (testID: string, large = false) => (
    <Pressable
      onPress={onSignIn}
      accessibilityRole="button"
      testID={testID}
      style={({ hovered, pressed }: { hovered?: boolean; pressed: boolean }) => [
        styles.signIn,
        large && styles.signInLarge,
        (hovered || pressed) && styles.signInActive,
      ]}
    >
      <LogIn size={16} color={theme.colors.accentForeground ?? "#ffffff"} strokeWidth={2} />
      <Text style={styles.signInText}>{t("conversation.signedOut.signIn")}</Text>
    </Pressable>
  );

  const main = (
    <View style={styles.main}>
      {compact ? (
        <View style={styles.compactHeader}>
          <Image source={YYC_LOGO} style={styles.headerLogo} accessibilityIgnoresInvertColors />
          <Text style={styles.headerTitle}>YYC AI</Text>
          <View style={styles.flex} />
          {signInButton("yyc-signed-out-header-sign-in")}
        </View>
      ) : null}
      <View style={[styles.center, compact && styles.centerCompact]}>
        <View style={styles.content}>
          <Image source={YYC_LOGO} style={styles.logo} accessibilityIgnoresInvertColors />
          <Text style={styles.greeting}>{t("conversation.signedOut.greeting")}</Text>
          <Text style={styles.lead}>{t("conversation.signedOut.lead")}</Text>
          <View style={styles.composer}>
            <TextInput
              editable={false}
              placeholder={t("conversation.signedOut.placeholder")}
              placeholderTextColor={muted}
              style={styles.input}
              multiline
              accessibilityLabel={t("conversation.signedOut.placeholder")}
            />
            <View style={styles.composerRow}>
              <Plus size={18} color={theme.colors.border} strokeWidth={1.5} />
              <View style={styles.flex} />
              {signInButton("yyc-signed-out-sign-in", true)}
            </View>
          </View>
        </View>
      </View>
    </View>
  );

  if (compact) return <View style={styles.screen}>{main}</View>;

  return (
    <View style={[styles.screen, styles.row]}>
      <View style={styles.sidebar}>
        <View style={styles.nav}>
          {([
            [Plus, t("conversation.signedOut.newChat")],
            [Trash2, t("conversation.signedOut.recentlyDeleted")],
            [Search, t("conversation.signedOut.search")],
          ] as const).map(([Icon, label]) => (
            <View key={label} style={styles.navRow} aria-disabled>
              <Icon size={16} color={theme.colors.border} strokeWidth={1.5} />
              <Text style={styles.navText}>{label}</Text>
            </View>
          ))}
        </View>
        <View style={styles.separator} />
        <Text style={styles.sectionTitle}>{t("conversation.signedOut.conversations")}</Text>
        <Text style={styles.sidebarNote}>{t("conversation.signedOut.sidebarNote")}</Text>
        <View style={styles.flex} />
        <View style={styles.sidebarFooter}>{signInButton("yyc-signed-out-sidebar-sign-in")}</View>
      </View>
      {main}
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  screen: {
    flex: 1,
    minHeight: "100%" as unknown as number,
    backgroundColor: theme.colors.surface0,
  },
  row: { flexDirection: "row" },
  flex: { flex: 1 },
  sidebar: {
    width: 308,
    backgroundColor: theme.colors.surfaceSidebar,
    borderRightWidth: 1,
    borderRightColor: theme.colors.border,
  },
  nav: { paddingVertical: theme.spacing[2], paddingHorizontal: theme.spacing[2] },
  navRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingHorizontal: theme.spacing[2],
    height: 29,
  },
  navText: { color: theme.colors.foregroundMuted, fontSize: theme.fontSize.base, opacity: 0.6 },
  separator: { height: 1, backgroundColor: theme.colors.border },
  sectionTitle: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
    paddingHorizontal: theme.spacing[4],
    paddingTop: theme.spacing[4],
    paddingBottom: theme.spacing[2],
  },
  sidebarNote: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    lineHeight: 21,
    paddingHorizontal: theme.spacing[4],
  },
  sidebarFooter: {
    borderTopWidth: 1,
    borderTopColor: theme.colors.border,
    padding: theme.spacing[3],
    alignItems: "stretch",
  },
  main: { flex: 1 },
  compactHeader: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    paddingHorizontal: theme.spacing[4],
    paddingVertical: theme.spacing[3],
  },
  headerLogo: { width: 28, height: 28, borderRadius: 7 },
  headerTitle: { color: theme.colors.foreground, fontSize: theme.fontSize.lg, fontWeight: theme.fontWeight.semibold },
  center: { flex: 1, justifyContent: "center", paddingHorizontal: theme.spacing[6] },
  centerCompact: { justifyContent: "flex-end", paddingHorizontal: theme.spacing[4], paddingBottom: theme.spacing[4] },
  content: { width: "100%", maxWidth: 760, alignSelf: "center" },
  logo: { width: 46, height: 46, borderRadius: 12, marginLeft: theme.spacing[2], marginBottom: theme.spacing[4] },
  greeting: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize["3xl"],
    marginLeft: theme.spacing[2],
    marginBottom: theme.spacing[2],
  },
  lead: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    lineHeight: 21,
    marginLeft: theme.spacing[2],
    marginBottom: theme.spacing[6],
  },
  composer: {
    borderWidth: 1,
    borderColor: theme.colors.border,
    borderRadius: theme.borderRadius["2xl"] + 4,
    backgroundColor: theme.colors.surface1,
    paddingHorizontal: theme.spacing[4],
    paddingTop: theme.spacing[3],
    paddingBottom: theme.spacing[3],
  },
  input: {
    minHeight: 44,
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.content,
  },
  composerRow: { flexDirection: "row", alignItems: "center", gap: theme.spacing[2], paddingTop: theme.spacing[2] },
  signIn: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "center",
    gap: theme.spacing[1.5],
    height: 34,
    paddingHorizontal: theme.spacing[4],
    borderRadius: theme.borderRadius.full,
    backgroundColor: theme.colors.accent,
  },
  signInLarge: { height: 38, paddingHorizontal: theme.spacing[6] },
  signInActive: { backgroundColor: theme.colors.accentBright },
  signInText: {
    color: theme.colors.accentForeground ?? "#ffffff",
    fontSize: theme.fontSize.base,
    fontWeight: theme.fontWeight.semibold,
  },
}));

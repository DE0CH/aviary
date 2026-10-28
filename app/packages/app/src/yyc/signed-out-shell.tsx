import React, { useCallback } from "react";
import { Image, Text, TextInput, View } from "react-native";
import { StyleSheet, useUnistyles } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import { LogIn, Plus, Search, Trash2 } from "lucide-react-native";
import { isWeb } from "@/constants/platform";
import { HEADER_INNER_HEIGHT, isCompactBreakpoint } from "@/constants/layout";
import { Button } from "@/components/ui/button";
import { DEFAULT_SIDEBAR_WIDTH } from "@/stores/panel-store/state";
import { density, densityIconBox, SIDEBAR_ROW_LINE_HEIGHT } from "@/styles/density";
import { ICON_SIZE } from "@/styles/theme";
import { YYC_HERO_LOGO_SIZE } from "@/yyc/hero";

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

  // The app's own button (as on the iOS sign-in screen), at the sidebar's control size.
  const signInButton = (testID: string) => (
    <Button variant="default" size="sm" leftIcon={LogIn} onPress={onSignIn} testID={testID}>
      {t("conversation.signedOut.signIn")}
    </Button>
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
      <View style={[styles.center, compact ? styles.centerCompact : styles.centerDesktop]}>
        <View style={styles.content}>
          <View style={styles.titleBlock}>
            <Image source={YYC_LOGO} style={styles.logo} accessibilityIgnoresInvertColors />
            <Text style={styles.greeting}>{t("conversation.signedOut.greeting")}</Text>
            <Text style={styles.lead}>{t("conversation.signedOut.lead")}</Text>
          </View>
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
              <View style={styles.composerButton}>
                <Plus size={isWeb ? ICON_SIZE.md : ICON_SIZE.lg} color={theme.colors.border} />
              </View>
              <View style={styles.flex} />
              {signInButton("yyc-signed-out-sign-in")}
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
        {/* The signed-in sidebar's rows (left-sidebar.tsx sidebarHeaderGroup, SidebarHeaderRow variant="compact"),
            below its 36 px chrome row, disabled. */}
        <View style={styles.chromeRow} />
        <View style={styles.nav}>
          {([
            [Plus, t("conversation.signedOut.newChat")],
            [Trash2, t("conversation.signedOut.recentlyDeleted")],
            [Search, t("conversation.signedOut.search")],
          ] as const).map(([Icon, label]) => (
            <View key={label} style={styles.navRow} aria-disabled>
              <View style={styles.navIconSlot}>
                <Icon size={ICON_SIZE.sm} color={theme.colors.border} />
              </View>
              <Text style={styles.navText}>{label}</Text>
            </View>
          ))}
        </View>
        <View style={styles.list}>
          <Text style={styles.sectionTitle}>{t("conversation.signedOut.conversations")}</Text>
          <Text style={styles.sidebarNote}>{t("conversation.signedOut.sidebarNote")}</Text>
        </View>
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
    width: DEFAULT_SIDEBAR_WIDTH,
    backgroundColor: theme.colors.surfaceSidebar,
    borderRightWidth: 1,
    borderRightColor: theme.colors.border,
  },
  chromeRow: { height: HEADER_INNER_HEIGHT },
  nav: {
    paddingHorizontal: theme.spacing[2],
    paddingBottom: theme.spacing[1.5],
    gap: 2,
    borderBottomWidth: 1,
    borderBottomColor: theme.colors.border,
  },
  navRow: {
    flexDirection: "row",
    alignItems: "center",
    gap: theme.spacing[2],
    minHeight: density.rowHeight,
    paddingVertical: theme.spacing[2],
    paddingHorizontal: theme.spacing[2],
    borderRadius: theme.borderRadius.lg,
  },
  navIconSlot: {
    width: densityIconBox("md"),
    height: densityIconBox("md"),
    alignItems: "center",
    justifyContent: "center",
  },
  navText: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    lineHeight: SIDEBAR_ROW_LINE_HEIGHT,
    opacity: 0.6,
  },
  // The conversation list's padding and its 「對話」 header (sidebar-section-header.tsx).
  list: { paddingHorizontal: theme.spacing[2], paddingTop: 2 },
  sectionTitle: {
    minHeight: density.rowHeight,
    lineHeight: density.rowHeight,
    paddingLeft: theme.spacing[2],
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.sm,
  },
  sidebarNote: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    lineHeight: SIDEBAR_ROW_LINE_HEIGHT,
    paddingHorizontal: theme.spacing[2],
  },
  sidebarFooter: {
    borderTopWidth: 1,
    borderTopColor: theme.colors.border,
    paddingHorizontal: theme.spacing[2],
    paddingVertical: theme.spacing[1.5],
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
  // The new-chat screen's layout (composer/dock/index.tsx, new-workspace-screen.tsx, composer/input/input.tsx).
  center: { flex: 1, justifyContent: "center", paddingHorizontal: theme.spacing[4] },
  centerDesktop: { paddingBottom: HEADER_INNER_HEIGHT + theme.spacing[6] },
  centerCompact: { justifyContent: "flex-end", paddingHorizontal: theme.spacing[4], paddingBottom: theme.spacing[4] },
  content: { width: "100%", maxWidth: theme.contentMaxWidth, alignSelf: "center" },
  titleBlock: { marginBottom: theme.spacing[8] },
  logo: {
    width: YYC_HERO_LOGO_SIZE,
    height: YYC_HERO_LOGO_SIZE,
    borderRadius: theme.borderRadius.xl,
    marginBottom: theme.spacing[4],
  },
  greeting: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize["2xl"],
    fontWeight: theme.fontWeight.normal,
    marginBottom: theme.spacing[2],
  },
  lead: {
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.base,
    lineHeight: 21,
  },
  composer: {
    borderWidth: 1,
    borderColor: theme.colors.borderAccent,
    borderRadius: theme.borderRadius["2xl"],
    backgroundColor: theme.colors.surface1,
    gap: theme.spacing[3],
    paddingTop: { xs: theme.spacing[2], md: theme.spacing[4] },
    paddingBottom: { xs: theme.spacing[2], md: theme.spacing[3] },
    paddingHorizontal: { xs: theme.spacing[3], md: theme.spacing[4] },
  },
  input: {
    minHeight: 46,
    color: theme.colors.foregroundMuted,
    fontSize: theme.fontSize.content,
    lineHeight: Math.round(theme.fontSize.content * 1.4),
  },
  composerRow: { flexDirection: "row", alignItems: "center", gap: theme.spacing[2] },
  composerButton: { width: 28, height: 28, alignItems: "center", justifyContent: "center" },
}));

import React, { useCallback, useEffect, useState, type ReactNode } from "react";
import { AppState, Image, Text, View } from "react-native";
import { StyleSheet } from "react-native-unistyles";
import { useTranslation } from "react-i18next";
import * as Crypto from "expo-crypto";
import * as WebBrowser from "expo-web-browser";
import { Button } from "@/components/ui/button";
import { LoadingSpinner } from "@/components/ui/loading-spinner";
import { isWeb } from "@/constants/platform";
import { accessHeaders, accessToken, loadAccessToken, saveAccessToken, subscribeAccessToken } from "@/yyc/access-token";
import { appScheme, siteOrigin } from "@/yyc/site";

const YYC_LOGO = require("../../assets/images/yyc-logo.png");

type GateState = "checking" | "signed-out" | "signed-in";

/** Whether the saved token still opens the site: Access answers an expired one with its login page, not JSON. */
async function tokenWorks(): Promise<boolean> {
  const origin = siteOrigin();
  if (!origin) return false;
  try {
    const response = await fetch(`${origin}/api/jarvis/me`, { headers: accessHeaders() });
    return response.ok && (response.headers.get("content-type") ?? "").includes("application/json");
  } catch {
    // offline: keep the token and let the app show it is reconnecting
    return true;
  }
}

/**
 * The iOS app signs in through Cloudflare Access before anything connects (docs/paseo-ui.md): the router's
 * /api/jarvis/app-signin opens Access's login in the system sign-in sheet (Google or an emailed code) and hands the
 * token back on the app's scheme. The web build is behind Access already and renders the app directly.
 */
export function AccessGate({ children }: { children: ReactNode }) {
  const [state, setState] = useState<GateState>(isWeb ? "signed-in" : "checking");

  useEffect(() => {
    if (isWeb) return;
    void (async () => {
      const token = await loadAccessToken();
      if (token && (await tokenWorks())) {
        setState("signed-in");
        return;
      }
      if (token) await saveAccessToken(null);
      setState("signed-out");
    })();
  }, []);

  // Signing out clears the token: back to the sign-in screen
  useEffect(() => {
    if (isWeb) return;
    return subscribeAccessToken(() => {
      if (!accessToken()) setState("signed-out");
    });
  }, []);

  // A token expires after 720 h: coming back to the app checks it again (at most hourly)
  useEffect(() => {
    if (isWeb || state !== "signed-in") return;
    let lastCheck = Date.now();
    const subscription = AppState.addEventListener("change", (next) => {
      if (next !== "active" || Date.now() - lastCheck < 60 * 60 * 1000) return;
      lastCheck = Date.now();
      void tokenWorks().then(async (works) => {
        if (works) return;
        await saveAccessToken(null);
        setState("signed-out");
      });
    });
    return () => subscription.remove();
  }, [state]);

  if (state === "signed-in") return <>{children}</>;
  if (state === "checking") {
    return (
      <View style={styles.screen}>
        <LoadingSpinner size="small" color="#0A7F95" />
      </View>
    );
  }
  return <SignInScreen onSignedIn={() => setState("signed-in")} />;
}

function SignInScreen({ onSignedIn }: { onSignedIn: () => void }) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const signIn = useCallback(async () => {
    const origin = siteOrigin();
    const scheme = appScheme();
    if (!origin || !scheme) {
      setError(t("conversation.signIn.notConfigured"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const state = Crypto.randomUUID();
      const result = await WebBrowser.openAuthSessionAsync(
        `${origin}/api/jarvis/app-signin?state=${encodeURIComponent(state)}`,
        `${scheme}://signed-in`,
      );
      if (result.type !== "success") return;
      const url = new URL(result.url);
      const token = new URLSearchParams(url.hash.replace(/^#/, "")).get("token");
      if (url.searchParams.get("state") !== state || !token) {
        setError(t("conversation.signIn.failed"));
        return;
      }
      await saveAccessToken(token);
      onSignedIn();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [onSignedIn, t]);

  return (
    <View style={styles.screen}>
      <Image source={YYC_LOGO} style={styles.logo} accessibilityIgnoresInvertColors />
      <Text style={styles.title}>{t("conversation.signIn.title")}</Text>
      <Button variant="default" size="md" onPress={() => void signIn()} disabled={busy} testID="yyc-sign-in">
        {t("conversation.signIn.button")}
      </Button>
      {error ? <Text style={styles.error}>{error}</Text> : null}
    </View>
  );
}

const styles = StyleSheet.create((theme) => ({
  screen: {
    flex: 1,
    alignItems: "center",
    justifyContent: "center",
    gap: theme.spacing[4],
    padding: theme.spacing[6],
    backgroundColor: theme.colors.surface0,
  },
  logo: {
    width: 64,
    height: 64,
    borderRadius: 17,
  },
  title: {
    color: theme.colors.foreground,
    fontSize: theme.fontSize.xl,
    fontWeight: theme.fontWeight.semibold,
  },
  error: {
    color: theme.colors.destructive,
    fontSize: theme.fontSize.base,
    textAlign: "center",
  },
}));

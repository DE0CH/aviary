/**
 * The logo in a native app (docs/paseo-ui.md "The iOS app"). The server's `/branding/logo.png` can sit behind
 * Cloudflare Access with everything else (Jarvis 4's chat does), so the app asks for it with its Access token, and
 * shows the logo its build carries while nobody is signed in. The web page is served its logo with the page.
 */
export interface BrandLogoInput {
  native: boolean;
  /** The server's logo (yyc/brand.ts); null when the server is not known. */
  uri: string | null;
  /** The headers that carry the Access token (yyc/access-token.ts); empty when signed out. */
  headers: Record<string, string>;
  /** The build's own logo (a bundled image); null when it has none. */
  bundled: number | null;
}

export type BrandLogoSource = number | { uri: string; headers?: Record<string, string> } | null;

export function brandLogoSource(input: BrandLogoInput): BrandLogoSource {
  if (!input.native) return input.uri ? { uri: input.uri } : null;
  const signedIn = Object.keys(input.headers).length > 0;
  if (signedIn && input.uri) return { uri: input.uri, headers: input.headers };
  return input.bundled ?? (input.uri ? { uri: input.uri } : null);
}

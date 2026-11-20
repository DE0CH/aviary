import { isWeb } from "@/constants/platform";
import { saveAccessToken } from "@/yyc/access-token";

/**
 * Ends the Cloudflare Access session; Access shows its sign-in again (to switch account, or leave). The iOS app
 * forgets its token and shows its sign-in screen (yyc/access-gate.tsx).
 */
export function signOut(): void {
  if (isWeb) {
    window.location.assign("/cdn-cgi/access/logout");
    return;
  }
  void saveAccessToken(null);
}

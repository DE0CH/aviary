import { useEffect, useState } from "react";
import { api } from "@/yyc/api";

interface Me {
  firstName: string | null;
  /** Sharing exists (sandbox mode); a Jarvis 4 session's local mode has none. */
  sharing: boolean;
}

let mePromise: Promise<Me> | null = null;

/** GET /api/jarvis/me (the router answers it in sandbox mode, the server in local mode), loaded once. */
function loadMe(): Promise<Me> {
  mePromise ??= Promise.race([
    api<{ firstName?: unknown; sharing?: unknown }>("/api/jarvis/me")
      .then((body) => ({
        firstName: typeof body?.firstName === "string" ? body.firstName : null,
        sharing: body?.sharing !== false,
      }))
      .catch(() => ({ firstName: null, sharing: true })),
    new Promise<Me>((resolve) => setTimeout(() => resolve({ firstName: null, sharing: true }), 3000)),
  ]);
  return mePromise;
}

function loadFirstName(): Promise<string | null> {
  return loadMe().then((me) => me.firstName);
}

/** Whether to offer sharing: true until the server says otherwise. */
export function useSharingAvailable(): boolean {
  const [sharing, setSharing] = useState(true);
  useEffect(() => {
    let live = true;
    void loadMe().then((me) => {
      if (live) setSharing(me.sharing);
    });
    return () => {
      live = false;
    };
  }, []);
  return sharing;
}

/** undefined until the answer is in (the greeting stays blank meanwhile, so it never swaps), then the name or null. */
export function useFirstName(): string | null | undefined {
  const [firstName, setFirstName] = useState<string | null | undefined>(undefined);
  useEffect(() => {
    let live = true;
    void loadFirstName().then((name) => {
      if (live) setFirstName(name);
    });
    return () => {
      live = false;
    };
  }, []);
  return firstName;
}

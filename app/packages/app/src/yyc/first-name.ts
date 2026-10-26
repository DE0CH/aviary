import { useEffect, useState } from "react";
import { api } from "@/yyc/api";

let firstNamePromise: Promise<string | null> | null = null;

/** The signed-in person's first name for the greeting (GET /api/jarvis/me, answered by the router), loaded once. */
function loadFirstName(): Promise<string | null> {
  firstNamePromise ??= Promise.race([
    api<{ firstName?: unknown }>("/api/jarvis/me")
      .then((body) => (typeof body?.firstName === "string" ? body.firstName : null))
      .catch(() => null),
    new Promise<null>((resolve) => setTimeout(() => resolve(null), 3000)),
  ]);
  return firstNamePromise;
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

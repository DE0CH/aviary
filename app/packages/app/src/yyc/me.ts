import { useEffect, useState } from "react";
import { api } from "@/yyc/api";

interface Me {
  firstName: string | null;
  /** Sharing exists (sandbox mode); a Jarvis 4 session's local mode has none. */
  sharing: boolean;
  /**
   * People start conversations here. False on a deployment that starts every conversation itself (Jarvis 4: each
   * session started in the Jarvis 4 app is one conversation; hermit's `conversations.peopleCreate`): no New chat.
   */
  newConversations: boolean;
  /**
   * The server takes a text message sent during a reply into the running turn (pigeon's `midTurn`; aviary magpie M7).
   * False (a server from before that): such a message waits in the composer's queue until the reply ends.
   */
  midTurn: boolean;
  /**
   * People pick each conversation's model and thinking level (pigeon's `modelPick`; hermit's
   * `conversations.peoplePickModel`). False: the model menu shows the choice and changes nothing.
   */
  modelPick: boolean;
}

const DEFAULT_ME: Me = {
  firstName: null,
  sharing: true,
  newConversations: true,
  midTurn: false,
  modelPick: false,
};
let loadedMe: Me | null = null;

let mePromise: Promise<Me> | null = null;

/** GET /api/jarvis/me (the router answers it in sandbox mode, the server in local mode), loaded once. */
function loadMe(): Promise<Me> {
  mePromise ??= Promise.race([
    api<{
      firstName?: unknown;
      sharing?: unknown;
      newConversations?: unknown;
      midTurn?: unknown;
      modelPick?: unknown;
    }>("/api/jarvis/me")
      .then((body) => {
        loadedMe = {
          firstName: typeof body?.firstName === "string" ? body.firstName : null,
          sharing: body?.sharing !== false,
          newConversations: body?.newConversations !== false,
          midTurn: body?.midTurn === true,
          modelPick: body?.modelPick === true,
        };
        return loadedMe;
      })
      .catch(() => DEFAULT_ME),
    new Promise<Me>((resolve) => setTimeout(() => resolve(DEFAULT_ME), 3000)),
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

/** Whether people pick a conversation's model and thinking level: false until the server says so (`modelPick`). */
export function useModelPickAvailable(): boolean {
  const [available, setAvailable] = useState(loadedMe?.modelPick ?? false);
  useEffect(() => {
    let live = true;
    void loadMe().then((me) => {
      if (live) setAvailable(me.modelPick);
    });
    return () => {
      live = false;
    };
  }, []);
  return available;
}

/** Whether people start conversations here: true until the server says otherwise (yyc/me.ts `newConversations`). */
export function useNewConversationsAvailable(): boolean {
  const [available, setAvailable] = useState(loadedMe?.newConversations ?? true);
  useEffect(() => {
    let live = true;
    void loadMe().then((me) => {
      if (live) setAvailable(me.newConversations);
    });
    return () => {
      live = false;
    };
  }, []);
  return available;
}

/** Whether a text message sent during a reply joins it: false until the server says so (yyc/me.ts `midTurn`). */
export function useMidTurnAvailable(): boolean {
  const [available, setAvailable] = useState(loadedMe?.midTurn ?? false);
  useEffect(() => {
    let live = true;
    void loadMe().then((me) => {
      if (live) setAvailable(me.midTurn);
    });
    return () => {
      live = false;
    };
  }, []);
  return available;
}

/** The same, read outside React (a keyboard action): true until `/api/jarvis/me` has answered otherwise. */
export function newConversationsAvailable(): boolean {
  void loadMe();
  return loadedMe?.newConversations ?? true;
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

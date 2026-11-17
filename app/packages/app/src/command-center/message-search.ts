import { useEffect, useState } from "react";
import { useDebouncedValue } from "@/hooks/use-debounced-value";
import { api, ApiError } from "@/yyc/api";
import {
  MESSAGE_SEARCH_DEBOUNCE_MS,
  MESSAGE_SEARCH_LIMIT,
  messageSearchQuery,
  usableHits,
  type MessageSearchBody,
  type MessageSearchHit,
} from "./message-search-model";

interface MessageSearchState {
  query: string | null;
  hits: readonly MessageSearchHit[];
  loading: boolean;
}

const IDLE: MessageSearchState = { query: null, hits: [], loading: false };

/** A server that answers 404 has no conversation search (an older pigeon): asked once per page. */
let unsupported = false;

/**
 * The conversation search for the palette's query, debounced; the hits of the latest query only. A failure shows
 * no hits (the palette still lists the conversations it matched by title).
 */
export function useMessageSearch(input: {
  enabled: boolean;
  query: string;
}): MessageSearchState {
  const debounced = useDebouncedValue(input.query, MESSAGE_SEARCH_DEBOUNCE_MS);
  const asked =
    input.enabled && !unsupported ? messageSearchQuery(debounced) : null;
  const [state, setState] = useState<MessageSearchState>(IDLE);

  useEffect(() => {
    if (!asked) return;
    let active = true;
    api<MessageSearchBody>("/api/jarvis/search", {
      method: "POST",
      body: JSON.stringify({ query: asked, limit: MESSAGE_SEARCH_LIMIT }),
    })
      .then((body) => {
        if (active)
          setState({
            query: asked,
            hits: usableHits(body, asked),
            loading: false,
          });
        return undefined;
      })
      .catch((error: unknown) => {
        if (error instanceof ApiError && error.status === 404)
          unsupported = true;
        if (active) setState({ query: asked, hits: [], loading: false });
      });
    return () => {
      active = false;
    };
  }, [asked]);

  // nothing to search, or what is typed now is too short: no hits; the asked query's answer not in yet: loading
  if (!asked || messageSearchQuery(input.query) === null) return IDLE;
  if (state.query !== asked) return { query: asked, hits: [], loading: true };
  return state;
}

import AsyncStorage from "@react-native-async-storage/async-storage";
import { z } from "zod";
import { create } from "zustand";
import { persist } from "zustand/middleware";
import { useShallow } from "zustand/react/shallow";
import { createValidatedPersistStorage } from "@/storage/validated-persist-storage";
import { useSessionStore } from "@/stores/session-store";
import { encodeImages } from "@/utils/encode-images";
import {
  DELIVERED_SHOWN_MS,
  afterLoad,
  drainOutbox,
  entriesFor,
  unsentEntries,
  type OutboxEntry,
} from "./outbox";

/**
 * The outbox as the app keeps it (yyc/outbox.ts for what it is): on the device, so a message the server has not
 * confirmed survives a reload or a closed app (Deyao, 2026-11-17), and sent again whenever the connection may be
 * back: when it is added, when the outbox is loaded, when the host comes online (host-runtime), and a while after a
 * send that failed on the connection.
 */

interface OutboxState {
  entries: OutboxEntry[];
}

const OutboxEntrySchema = z.object({
  id: z.string(),
  serverId: z.string(),
  agentId: z.string(),
  text: z.string(),
  images: z.array(z.any()),
  attachments: z.array(z.any()),
  activeTurnBehavior: z.enum(["interrupt", "steer"]).optional(),
  queued: z.boolean(),
  createdAtMs: z.number(),
  deliveredAtMs: z.number().optional(),
});

const OutboxPersistedSchema = z.object({ entries: z.array(OutboxEntrySchema) });

export const useOutboxStore = create<OutboxState>()(
  persist(() => ({ entries: [] as OutboxEntry[] }), {
    name: "yyc-outbox",
    version: 1,
    storage: createValidatedPersistStorage(
      AsyncStorage,
      OutboxPersistedSchema as unknown as z.ZodType<OutboxState>,
    ),
    partialize: ({ entries }) => ({ entries }),
    merge: (persisted, current) => {
      const loaded = (persisted as OutboxState | undefined)?.entries ?? [];
      // what this page added before the load finished stays, beside what the device kept
      const ids = new Set(current.entries.map((e) => e.id));
      return { ...current, entries: [...afterLoad(loaded).filter((e) => !ids.has(e.id)), ...current.entries] };
    },
    onRehydrateStorage: () => () => {
      for (const serverId of new Set(useOutboxStore.getState().entries.map((e) => e.serverId))) {
        drainServerOutbox(serverId);
      }
    },
  }),
);

function update(fn: (entries: OutboxEntry[]) => OutboxEntry[]): void {
  useOutboxStore.setState((state) => {
    const entries = fn(state.entries);
    return entries === state.entries ? state : { entries };
  });
}

/** A message that left the outbox without reaching the server: the message box takes it back. */
export interface OutboxReturn {
  entry: OutboxEntry;
  reason: "refused" | "cancelled";
  error?: Error;
}

const returnListeners = new Set<(ret: OutboxReturn) => void>();

export function subscribeOutboxReturns(listener: (ret: OutboxReturn) => void): () => void {
  returnListeners.add(listener);
  return () => returnListeners.delete(listener);
}

function emitReturn(ret: OutboxReturn): void {
  for (const listener of returnListeners) listener(ret);
}

/** Puts a message into the outbox and starts sending it. */
export function addToOutbox(entry: OutboxEntry): void {
  update((entries) => [...entries.filter((e) => e.id !== entry.id), entry]);
  drainServerOutbox(entry.serverId);
}

function markDelivered(id: string): void {
  const nowMs = Date.now();
  update((entries) =>
    entries
      .map((e) => (e.id === id ? { ...e, deliveredAtMs: nowMs } : e))
      // the ones delivered earlier have had their rows' time
      .filter((e) => e.deliveredAtMs === undefined || nowMs - e.deliveredAtMs < DELIVERED_SHOWN_MS),
  );
}

function removeEntry(id: string): void {
  update((entries) => {
    const next = entries.filter((e) => e.id !== id);
    return next.length === entries.length ? entries : next;
  });
}

/**
 * Takes back a message the server has not confirmed (its 取消): it leaves the outbox and the chat, and the message box
 * gets its text again. A send already on its way may still arrive; then its row comes as any other.
 */
export function cancelOutboxEntry(id: string): void {
  const entry = useOutboxStore.getState().entries.find((e) => e.id === id);
  if (!entry || entry.deliveredAtMs !== undefined) return;
  removeEntry(id);
  useSessionStore.getState().rejectAgentMessageSubmission(entry.serverId, entry.agentId, id);
  emitReturn({ entry, reason: "cancelled" });
}

/** How long after a send that failed on the connection the outbox tries again by itself. */
export const OUTBOX_RETRY_MS = 15_000;

const draining = new Set<string>();
/** Servers whose outbox was asked to send while it was sending (the connection came back meanwhile). */
const askedAgain = new Set<string>();
const retryTimers = new Map<string, ReturnType<typeof setTimeout>>();

/** Sends a server's unsent messages in order, once at a time per server. Safe to call whenever. */
export function drainServerOutbox(serverId: string): void {
  if (draining.has(serverId)) {
    askedAgain.add(serverId);
    return;
  }
  const client = useSessionStore.getState().sessions[serverId]?.client;
  if (!client) return;
  const pending = unsentEntries(useOutboxStore.getState().entries, serverId);
  if (pending.length === 0) return;
  const timer = retryTimers.get(serverId);
  if (timer !== undefined) {
    clearTimeout(timer);
    retryTimers.delete(serverId);
  }
  draining.add(serverId);
  void drainOutbox({
    entries: pending,
    send: async (entry) => {
      // a message taken back while the ones before it were sent is not sent
      if (!useOutboxStore.getState().entries.some((e) => e.id === entry.id)) return;
      const images = await encodeImages(entry.images);
      await client.sendAgentMessage(entry.agentId, entry.text, {
        messageId: entry.id,
        ...(entry.activeTurnBehavior ? { activeTurnBehavior: entry.activeTurnBehavior } : {}),
        images: images ?? [],
        attachments: entry.attachments,
      });
    },
    onDelivered: (entry) => {
      if (!useOutboxStore.getState().entries.some((e) => e.id === entry.id)) return;
      markDelivered(entry.id);
      useSessionStore.getState().acceptAgentMessageSubmission(serverId, entry.agentId, entry.id);
    },
    onRefused: (entry, error) => {
      console.error("[Outbox] the server refused a message", { serverId, agentId: entry.agentId, error });
      removeEntry(entry.id);
      useSessionStore.getState().rejectAgentMessageSubmission(serverId, entry.agentId, entry.id);
      emitReturn({ entry, reason: "refused", error });
    },
  })
    .then((result) => {
      draining.delete(serverId);
      const again = askedAgain.delete(serverId);
      if (result === "offline" && !again) {
        retryTimers.set(
          serverId,
          setTimeout(() => {
            retryTimers.delete(serverId);
            drainServerOutbox(serverId);
          }, OUTBOX_RETRY_MS),
        );
        return;
      }
      // messages added while this run sent the others
      if (unsentEntries(useOutboxStore.getState().entries, serverId).length > 0) {
        drainServerOutbox(serverId);
      }
    })
    .catch((error) => {
      draining.delete(serverId);
      console.error("[Outbox] sending failed", { serverId, error });
    });
}

/** The conversation's outbox messages, in the order they were sent (a new array only when they change). */
export function useOutboxEntries(serverId: string, agentId: string | null | undefined): OutboxEntry[] {
  return useOutboxStore(
    useShallow((state) => (agentId ? entriesFor(state.entries, serverId, agentId) : NO_ENTRIES)),
  );
}

const NO_ENTRIES: OutboxEntry[] = [];

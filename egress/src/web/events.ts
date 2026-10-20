// Turn events: appended to Postgres (the one source of truth every replica streams from) and announced with
// pg_notify('conv_<id>', '<event id>'). seq is per turn; writers in several processes (the turn runner, the tools
// role through /internal/events) take the next free one and retry on a clash.
import { channel, type Db } from "../common/db.ts";

export interface EventRow {
  id: string;
  conversation_id: string;
  turn_id: string;
  seq: number;
  type: string;
  data: Record<string, unknown>;
  created_at: Date;
}

export async function appendEvent(db: Db, conversationId: string, turnId: string, type: string, data: Record<string, unknown>): Promise<string> {
  for (let attempt = 0; ; attempt++) {
    try {
      const { rows } = await db.query<{ id: string }>(
        `insert into events (conversation_id, turn_id, seq, type, data)
         select $1, $2, coalesce(max(seq), 0) + 1, $3, $4 from events where turn_id = $2
         returning id`,
        [conversationId, turnId, type, JSON.stringify(data)],
      );
      const id = rows[0]!.id;
      await db.query("select pg_notify($1, $2)", [channel("conv", conversationId), id]);
      return id;
    } catch (e) {
      if ((e as { code?: string }).code === "23505" && attempt < 20) continue;
      throw e;
    }
  }
}

export function publicEvent(r: EventRow): Record<string, unknown> {
  return { id: Number(r.id), turnId: r.turn_id, seq: r.seq, type: r.type, data: r.data, createdAt: r.created_at };
}

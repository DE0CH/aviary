// Outward actions wait for the staff member (docs/CHAT.md, "Outward actions need the staff member's confirmation"):
// a confirmations row, a confirmation_request event through web (the UI shows approve/deny), then the answer arrives
// as NOTIFY confirm_<id> (web's POST /api/confirmations/:id sets the row). No answer in time = expired.
import { randomUUID } from "node:crypto";
import { channel } from "../common/db.ts";
import { logError } from "../common/log.ts";
import type { ToolCtx } from "./types.ts";

export type Answer = "approved" | "denied" | "expired";

export async function postEvent(ctx: ToolCtx, event: Record<string, unknown>): Promise<void> {
  const r = await ctx.deps.fetch(`${ctx.deps.webUrl}/internal/events`, {
    method: "POST",
    headers: { authorization: `Bearer ${ctx.token}`, "content-type": "application/json" },
    body: JSON.stringify(event),
  });
  if (!r.ok) throw new Error(`web /internal/events answered ${r.status}`);
}

export async function askUser(ctx: ToolCtx, kind: string, summary: string, detail: Record<string, unknown>): Promise<Answer> {
  const { db, listener, confirmTimeoutMs } = ctx.deps;
  const id = randomUUID();
  await db.query(
    `insert into confirmations (id, conversation_id, turn_id, kind, summary, detail, status) values ($1,$2,$3,$4,$5,$6,'pending')`,
    [id, ctx.turn.conversation_id, ctx.turn.id, kind, summary, JSON.stringify(detail)],
  );
  const read = async (): Promise<Answer | null> => {
    const { rows } = await db.query<{ status: string }>("select status from confirmations where id = $1", [id]);
    const s = rows[0]?.status;
    return s === "approved" || s === "denied" || s === "expired" ? s : null;
  };
  let wake: () => void = () => {};
  const unlisten = await listener.listen(channel("confirm", id), () => wake());
  try {
    await postEvent(ctx, { type: "confirmation_request", id, kind, summary, detail });
    const deadline = Date.now() + confirmTimeoutMs;
    for (;;) {
      const s = await read();
      if (s) return s;
      const left = deadline - Date.now();
      if (left <= 0) break;
      // woken by NOTIFY (or by a listener reconnect); a 30 s re-read covers a NOTIFY lost in between
      await new Promise<void>((r) => {
        const t = setTimeout(r, Math.min(left, 30_000));
        wake = () => {
          clearTimeout(t);
          r();
        };
      });
    }
    const { rowCount } = await db.query(
      "update confirmations set status = 'expired', decided_at = now() where id = $1 and status = 'pending'",
      [id],
    );
    if (!rowCount) return (await read()) ?? "expired";
    await postEvent(ctx, { type: "confirmation_resolved", id, status: "expired" }).catch((e) => logError("confirm expired event", e, { id }));
    return "expired";
  } finally {
    await unlisten();
  }
}

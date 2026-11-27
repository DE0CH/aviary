// An answer is as old as its question (aviary platform spec 3.11.2 step 3; 2026-11-26): hermit's liveness answer
// counts its 30 s from when the lookup was sent, however long the answer took. Stamped on arrival, an answer whose
// lookup the clock moved past hid what hermit said next for another whole TTL (aviary's conformance case
// liveness.revoked, 1 run in 10). Both views here, the gateway's (egress) and tools', with a hermit held by hand and
// a clock moved by hand: no timing, only order.
import assert from "node:assert/strict";
import { test } from "node:test";
import { SandboxTokens, type HermitGet } from "../src/egress/tokens.ts";
import { Liveness } from "../src/tools/liveness.ts";

const ID = "0190d1f0-0000-7000-8000-000000000001";
const CONVERSATION = "0190d1f0-0000-7000-8000-000000000002";
const PERSON = "staff@example.org";

/** a hermit whose liveness answers are sent by hand: each lookup waits for `answer` */
function heldHermit() {
  const waiting: ((live: boolean) => void)[] = [];
  let lookups = 0;
  const get: HermitGet = (path) => {
    assert.equal(path, `/v1/sandbox-tokens/${ID}`);
    lookups++;
    return new Promise((resolve) =>
      waiting.push((live) =>
        resolve([200, JSON.stringify({ sandboxId: ID, conversationId: CONVERSATION, person: PERSON, state: live ? "ready" : "gone", live })]),
      ),
    );
  };
  return {
    get,
    lookups: () => lookups,
    /** sends the oldest held lookup's answer, then lets its continuations run */
    async answer(live: boolean) {
      const r = waiting.shift();
      assert.ok(r, "no lookup is waiting");
      r(live);
      for (let i = 0; i < 5; i++) await Promise.resolve();
      await new Promise((r) => setImmediate(r));
    },
  };
}

test("gateway: an answer whose lookup outlived the TTL is old, and the next request asks again", async () => {
  const h = heldHermit();
  let clock = 0;
  const st = new SandboxTokens(h.get, { now: () => clock });
  assert.equal(st.admits(ID), true, "no answer yet: admitted on the token");
  assert.equal(h.lookups(), 1);
  // the whole TTL passes while the lookup is out; then it answers live (as hermit stood when asked)
  clock += 30_000;
  await h.answer(true);
  // hermit has said nothing since it was asked, 30 s ago: the next request asks again
  assert.equal(st.admits(ID), true, "the old answer still admits while the new lookup runs");
  assert.equal(h.lookups(), 2, "an answer asked for 30 s ago is due for a refresh");
  await h.answer(false);
  assert.equal(st.admits(ID), false, "hermit's live: false is seen at once");
});

test("tools: an answer whose lookup outlived the TTL is old, and the next call asks again", async () => {
  const h = heldHermit();
  let clock = 0;
  const lv = new Liveness(h.get, () => clock);
  // the first lookup, answered at once (the first call waits for it)
  const first = lv.check(ID, CONVERSATION, PERSON);
  await h.answer(true);
  assert.deepEqual(await first, { ok: true, stale: false });
  // past the TTL: a call refreshes in the background, and the TTL passes again while that lookup is out
  clock += 30_000;
  assert.deepEqual(await lv.check(ID, CONVERSATION, PERSON), { ok: true, stale: true });
  assert.equal(h.lookups(), 2);
  clock += 30_000;
  await h.answer(true);
  // that answer was asked for 30 s ago: the next call asks again, and hermit's refusal lands
  await lv.check(ID, CONVERSATION, PERSON);
  assert.equal(h.lookups(), 3, "an answer asked for 30 s ago is due for a refresh");
  await h.answer(false);
  assert.equal((await lv.check(ID, CONVERSATION, PERSON)).ok, false, "hermit's live: false is seen at once");
});

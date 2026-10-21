// Packing as a pure function (src/web/packing.ts): current placement + host ages + slots -> moves.
import assert from "node:assert/strict";
import { test } from "node:test";
import { planPacking, type PackHost, type PackSandbox } from "../src/web/packing.ts";

const host = (name: string, slots = 14, paid = false, open = true): PackHost => ({ name, slots, paid, open });
const on = (h: string, n: number, size = 1): PackSandbox[] => Array.from({ length: n }, (_, i) => ({ id: `${h}-${i}`, host: h, size }));
const after = (sbx: PackSandbox[], moves: { id: string; to: string }[]) => {
  const where = new Map(sbx.map((s) => [s.id, s.host]));
  for (const m of moves) where.set(m.id, m.to);
  return where;
};

test("three hosts partly full, all past their minimum lifetime: everything lands on the fullest, two hosts empty", () => {
  const hosts = [host("a"), host("b"), host("c")];
  const sbx = [...on("a", 5), ...on("b", 3), ...on("c", 2)];
  const moves = planPacking(hosts, sbx);
  assert.equal(moves.length, 5);
  assert.deepEqual(new Set(after(sbx, moves).values()), new Set(["a"]));
});

test("more than one host's worth: the fewest hosts that hold them", () => {
  const hosts = [host("a", 4), host("b", 4), host("c", 4)];
  const sbx = [...on("a", 3), ...on("b", 2), ...on("c", 2)];
  const used = new Set(after(sbx, planPacking(hosts, sbx)).values());
  assert.equal(used.size, 2);
});

test("hosts still within their minimum lifetime are free to use: filled first, never emptied for nothing", () => {
  // the new host is paid for: the old host's sandbox goes there, the old host empties
  const sbx = [...on("old", 1), ...on("new", 1)];
  assert.deepEqual(planPacking([host("old"), host("new", 14, true)], sbx), [{ id: "old-0", from: "old", to: "new" }]);
  // only paid hosts in use: nothing to gain
  assert.deepEqual(planPacking([host("p1", 14, true), host("p2", 14, true)], [...on("p1", 1), ...on("p2", 1)]), []);
});

test("no move without fewer releasable hosts: full hosts, a sandbox that fits nowhere, an empty or closed host", () => {
  assert.deepEqual(planPacking([host("a", 4), host("b", 4)], [...on("a", 3), ...on("b", 3)]), []);
  // a 2-slot sandbox (made before a smaller sandbox.memory) needs 2 free slots
  assert.deepEqual(planPacking([host("a", 4), host("b", 4)], [...on("a", 3), ...on("b", 1, 2)]), []);
  // an empty host past its minimum lifetime is never a target (it goes by the release rule)
  assert.deepEqual(planPacking([host("a"), host("empty")], on("a", 3)), []);
  // a host short of memory takes nothing
  assert.deepEqual(planPacking([host("a", 14, false, false), host("b")], [...on("a", 1), ...on("b", 1)]), [{ id: "a-0", from: "a", to: "b" }]);
});

test("tie-breaker: of two equally good plans, the one that moves the sandboxes moved least recently", () => {
  const sbx = [{ id: "x", host: "a", size: 1, movedAt: 1000 }, { id: "y", host: "b", size: 1, movedAt: 0 }];
  assert.deepEqual(planPacking([host("a"), host("b")], sbx), [{ id: "y", from: "b", to: "a" }]);
});

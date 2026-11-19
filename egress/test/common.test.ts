import assert from "node:assert/strict";
import { test } from "node:test";
import { expandEnv } from "../src/common/config.ts";

test("expandEnv refuses unset variables", () => {
  assert.equal(expandEnv("Bearer ${K}", { K: "v" }), "Bearer v");
  assert.throws(() => expandEnv("${NOPE}", {}), /NOPE/);
});

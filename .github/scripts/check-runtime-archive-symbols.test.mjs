import assert from "node:assert/strict";
import test from "node:test";

import {
  findRemovedRuntimeSymbols,
  removedRuntimeSymbols,
} from "./check-runtime-archive-symbols.mjs";

test("accepts the live typed and shared Task runtime boundary", () => {
  const archive = Buffer.from(
    [
      "loom_gc_typed_alloc_v1",
      "loom_gc_typed_root_push_v1",
      "loom_task_prepare_join",
      "loom_task_join_step",
      "loom_task_report_fault",
      "notloom_gc_root_push_v1",
    ].join("\0"),
    "ascii",
  );

  assert.deepEqual(findRemovedRuntimeSymbols(archive), []);
});

test("reports every removed universal runtime symbol", () => {
  for (const symbol of removedRuntimeSymbols) {
    const archive = Buffer.from(`_${symbol}\0`, "ascii");
    assert.deepEqual(findRemovedRuntimeSymbols(archive), [symbol], symbol);
  }
});

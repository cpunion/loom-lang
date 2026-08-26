import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { readReport, renderComment } from "./render-benchmark-comment.mjs";

function report(median, compileMs = 10) {
  return {
    os: "linux",
    architecture: "x86_64",
    profile: "throughput",
    warmups: 2,
    measuredRuns: 5,
    cases: new Map([
      ["integer", { scale: 100, results: new Map([["loom", median]]) }],
    ]),
    toolchains: new Map([
      ["loom", { compileMs, binaryBytes: 4096, sourceSha256: "a".repeat(64) }],
    ]),
  };
}

test("renders base and candidate deltas", () => {
  const output = renderComment(report(10), report(9, 11), "0123456789abcdef");
  assert.match(output, /Candidate `0123456789ab`/);
  assert.match(output, /9\.000 ms \| -10\.0%/);
  assert.match(output, /11\.00 ms \| \+10\.0%/);
});

test("rejects incomparable profiles", () => {
  const candidate = report(9);
  candidate.profile = "quick";
  assert.throws(
    () => renderComment(report(10), candidate, "0123456789abcdef"),
    /same host profile/,
  );
});

test("rejects a spoofed head identity", () => {
  assert.throws(
    () => renderComment(report(10), report(9), "@maintainer"),
    /head SHA is malformed/,
  );
});

test("rejects an unvalidated report identity", (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "loom-benchmark-report-"));
  context.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const filename = path.join(directory, "report.json");
  fs.writeFileSync(filename, JSON.stringify({ schemaVersion: 1, kind: "spoofed" }));
  assert.throws(() => readReport(filename, "candidate"), /unsupported identity/);
});

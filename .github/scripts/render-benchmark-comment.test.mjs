import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  readComparisons,
  readReport,
  renderComment,
} from "./render-benchmark-comment.mjs";

function validatedReport(platform, median, compileMs = 10) {
  const [osName, architecture] = platform.split("/");
  return {
    os: osName,
    architecture,
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

function rawReport(platform, median, compileMs = 10) {
  const [osName, architecture] = platform.split("/");
  return {
    schemaVersion: 1,
    kind: "loom-cross-language-basic-benchmark",
    status: "passed",
    host: { os: osName, architecture },
    config: { profile: "throughput", warmups: 2, measuredRuns: 5 },
    toolchains: [
      {
        language: "loom",
        compileMs,
        binaryBytes: 4096,
        sourceSha256: "a".repeat(64),
      },
    ],
    cases: [
      {
        name: "integer",
        scale: 100,
        results: [{ language: "loom", medianMs: median }],
      },
    ],
  };
}

function comparison(platform, baseMedian, candidateMedian, candidateCompileMs = 11) {
  const base = validatedReport(platform, baseMedian);
  const candidate = validatedReport(platform, candidateMedian, candidateCompileMs);
  return {
    key: platform,
    profile: candidate.profile,
    warmups: candidate.warmups,
    measuredRuns: candidate.measuredRuns,
    runtime: new Map([
      [
        "integer\0loom",
        {
          caseName: "integer",
          language: "loom",
          scale: 100,
          base: baseMedian,
          candidate: candidateMedian,
        },
      ],
    ]),
    tools: new Map([
      ["loom", { base: base.toolchains.get("loom"), candidate: candidate.toolchains.get("loom") }],
    ]),
  };
}

function evidenceFixture(runId, reports) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "loom-benchmark-evidence-"));
  for (const [platform, [base, candidate]] of Object.entries(reports)) {
    const directory = path.join(root, `pr-benchmark-${runId}-${platform.replace("/", "-")}`);
    fs.mkdirSync(directory);
    fs.writeFileSync(path.join(directory, "benchmark-base.json"), JSON.stringify(base));
    fs.writeFileSync(path.join(directory, "benchmark-candidate.json"), JSON.stringify(candidate));
  }
  return root;
}

test("renders exactly one horizontal table for macOS, Linux, and unavailable Windows", () => {
  const output = renderComment(
    [comparison("linux/x86_64", 10, 9), comparison("macos/aarch64", 20, 18)],
    "0123456789abcdef",
  );
  assert.match(output, /Candidate `0123456789ab`/);
  assert.match(output, /macOS \(base \\\| candidate \\\| delta\).*Linux.*Windows/);
  assert.match(output, /20\.000 ms \\\| 18\.000 ms \\\| -10\.0%/);
  assert.match(output, /10\.000 ms \\\| 9\.000 ms \\\| -10\.0%/);
  assert.match(output, /— \(frontend only\)/);
  assert.match(output, /10\.00 ms \\\| 11\.00 ms \\\| \+10\.0%/);
  assert.match(output, /4096 B \\\| 4096 B \\\| 0\.0%/);
  assert.equal(output.match(/^\| ---/gm)?.length, 1);
  assert.doesNotMatch(output, /<details>/);
});

test("rejects platform reports that use different profiles", () => {
  const macos = comparison("macos/aarch64", 20, 18);
  macos.profile = "quick";
  assert.throws(
    () => renderComment([comparison("linux/x86_64", 10, 9), macos], "0123456789abcdef"),
    /same benchmark suite and profile/,
  );
});

test("rejects a spoofed head identity", () => {
  assert.throws(
    () => renderComment([comparison("linux/x86_64", 10, 9)], "@maintainer"),
    /head SHA is malformed/,
  );
});

test("discovers only bounded artifacts from the requested workflow run", (context) => {
  const root = evidenceFixture("42", {
    "linux/x86_64": [rawReport("linux/x86_64", 10), rawReport("linux/x86_64", 9)],
    "macos/aarch64": [rawReport("macos/aarch64", 20), rawReport("macos/aarch64", 18)],
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));

  const comparisons = readComparisons(root, "42");
  assert.deepEqual(
    comparisons.map((entry) => entry.key),
    ["linux/x86_64", "macos/aarch64"],
  );
});

test("rejects extra files in an untrusted artifact", (context) => {
  const root = evidenceFixture("42", {
    "linux/x86_64": [rawReport("linux/x86_64", 10), rawReport("linux/x86_64", 9)],
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const directory = path.join(root, "pr-benchmark-42-linux-x86_64");
  fs.writeFileSync(path.join(directory, "payload.js"), "throw new Error('must not run');\n");

  assert.throws(() => readComparisons(root, "42"), /exactly one base and one candidate/);
});

test("rejects an unvalidated report identity", (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "loom-benchmark-report-"));
  context.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const filename = path.join(directory, "report.json");
  fs.writeFileSync(filename, JSON.stringify({ schemaVersion: 1, kind: "spoofed" }));
  assert.throws(() => readReport(filename, "candidate"), /unsupported identity/);
});

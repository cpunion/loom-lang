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
    [comparison("linux/x86_64", 10, 11), comparison("macos/aarch64", 20, 18)],
    "0123456789abcdef",
  );
  assert.match(output, /Candidate `0123456789ab`/);
  assert.match(output, /macOS \(base \\\| candidate \\\| delta\).*Linux.*Windows/);
  assert.match(output, /20\.000 ms \\\| 18\.000 ms \\\| -10\.0%/);
  assert.match(output, /10\.000 ms \\\| 11\.000 ms \\\| \+10\.0%/);
  assert.match(output, /— \(frontend only\)/);
  assert.match(output, /10\.00 ms \\\| 11\.00 ms \\\| \+10\.0%/);
  assert.match(output, /4096 B \\\| 4096 B \\\| 0\.0%/);
  assert.equal(output.match(/^\| ---/gm)?.length, 1);
  assert.doesNotMatch(output, /<details>/);
  assert.doesNotMatch(output, /<summary>/);
  assert.equal(output.match(/^```mermaid$/gm)?.length, 1);
  assert.match(output, /### Runtime comparison chart/);
  assert.match(output, /macOS = blue bars; Linux = orange line/);
  assert.match(output, /plotColorPalette: "#0969da, #59636e, #eb670f"/);
  assert.match(output, /title "Runtime index by platform \(base = 100\)"/);
  assert.match(output, /x-axis \["base", "integer\/loom"\]/);
  assert.match(output, /y-axis "Runtime index" 0 --> 120/);
  assert.match(output, /bar \[100, 90\]\n  line \[100, 100\]\n  line \[100, 110\]/);
  assert.ok(output.indexOf("| Case |") < output.indexOf("### Runtime comparison chart"));
});

test("renders only the measured platform series", () => {
  const output = renderComment(
    [comparison("linux/x86_64", 10, 9)],
    "0123456789abcdef",
  );
  assert.equal(output.match(/^```mermaid$/gm)?.length, 1);
  assert.match(output, /Linux = orange line/);
  assert.doesNotMatch(output, /macOS = blue bars/);
  assert.match(output, /plotColorPalette: "#59636e, #eb670f"/);
  assert.doesNotMatch(output, /^  bar \[/m);
  assert.equal(output.match(/^  line \[/gm)?.length, 2);
  assert.match(output, /line \[100, 100\]\n  line \[100, 90\]/);
});

test("renders a macOS-only chart with a visible base-line anchor", () => {
  const output = renderComment(
    [comparison("macos/aarch64", 10, 9)],
    "0123456789abcdef",
  );
  assert.equal(output.match(/^```mermaid$/gm)?.length, 1);
  assert.match(output, /macOS = blue bars/);
  assert.doesNotMatch(output, /Linux = orange line/);
  assert.match(output, /plotColorPalette: "#0969da, #59636e"/);
  assert.match(output, /bar \[100, 90\]\n  line \[100, 100\]/);
});

test("omits the chart when no chartable platform evidence exists", () => {
  const output = renderComment(
    [comparison("windows/x86_64", 10, 9)],
    "0123456789abcdef",
  );
  assert.doesNotMatch(output, /### Runtime comparison chart/);
  assert.doesNotMatch(output, /^```mermaid$/m);
});

test("uses one runtime-index scale for both measured platform series", () => {
  const output = renderComment(
    [comparison("macos/aarch64", 10, 9), comparison("linux/x86_64", 10, 15)],
    "0123456789abcdef",
  );
  assert.equal(output.match(/y-axis "Runtime index" 0 --> 160/g)?.length, 1);
  assert.match(output, /bar \[100, 90\]/);
  assert.match(output, /line \[100, 150\]/);
});

test("aligns every platform series to the same runtime keys", () => {
  const macos = comparison("macos/aarch64", 20, 18);
  const linux = comparison("linux/x86_64", 10, 11);
  macos.runtime.set("fib\0loom", {
    caseName: "fib",
    language: "loom",
    scale: 30,
    base: 25,
    candidate: 30,
  });
  linux.runtime.set("fib\0loom", {
    caseName: "fib",
    language: "loom",
    scale: 30,
    base: 25,
    candidate: 20,
  });

  const output = renderComment([linux, macos], "0123456789abcdef");
  assert.match(output, /x-axis \["integer\/loom", "fib\/loom"\]/);
  assert.match(
    output,
    /bar \[90, 120\]\n  line \[100, 100\]\n  line \[110, 80\]/,
  );
});

test("draws Linux after the base line so unchanged results remain visible", () => {
  const macos = comparison("macos/aarch64", 10, 9);
  const linux = comparison("linux/x86_64", 10, 10);
  macos.runtime.set("fib\0loom", {
    caseName: "fib",
    language: "loom",
    scale: 30,
    base: 20,
    candidate: 18,
  });
  linux.runtime.set("fib\0loom", {
    caseName: "fib",
    language: "loom",
    scale: 30,
    base: 20,
    candidate: 20,
  });

  const output = renderComment([macos, linux], "0123456789abcdef");
  assert.match(output, /plotColorPalette: "#0969da, #59636e, #eb670f"/);
  assert.match(output, /bar \[90, 90\]\n  line \[100, 100\]\n  line \[100, 100\]/);
});

test("rejects a comparison whose percentage delta overflows", () => {
  assert.throws(
    () =>
      renderComment(
        [comparison("linux/x86_64", Number.MIN_VALUE, Number.MAX_VALUE)],
        "0123456789abcdef",
      ),
    /comparison delta must be finite/,
  );
});

test("rejects an unbounded runtime chart index", () => {
  assert.throws(
    () => renderComment([comparison("linux/x86_64", 1, 101)], "0123456789abcdef"),
    /runtime chart index exceeds the supported limit/,
  );
});

test("rejects a runtime chart with too many entries", () => {
  const linux = comparison("linux/x86_64", 10, 9);
  for (let index = 0; index < 64; index += 1) {
    linux.runtime.set(`case${index}\0loom`, {
      caseName: `case${index}`,
      language: "loom",
      scale: 100,
      base: 10,
      candidate: 9,
    });
  }
  assert.throws(
    () => renderComment([linux], "0123456789abcdef"),
    /too many runtime entries for a bounded chart/,
  );
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

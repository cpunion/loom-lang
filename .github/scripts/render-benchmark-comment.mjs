import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const MAX_REPORT_BYTES = 2 * 1024 * 1024;
const MAX_CASES = 32;
const MAX_LANGUAGES = 16;
const REPORT_KIND = "loom-cross-language-basic-benchmark";
const SAFE_NAME = /^[A-Za-z0-9_.+()-]{1,64}$/;
const SHA256 = /^[0-9a-f]{64}$/i;

function fail(message) {
  throw new Error(`benchmark report rejected: ${message}`);
}

function finiteNumber(value, label, { positive = false } = {}) {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    fail(`${label} must be a finite number`);
  }
  if (positive && value <= 0) {
    fail(`${label} must be greater than zero`);
  }
  return value;
}

function safeName(value, label) {
  if (typeof value !== "string" || !SAFE_NAME.test(value)) {
    fail(`${label} is not a bounded portable name`);
  }
  return value;
}

function validateReport(report, label) {
  if (report === null || typeof report !== "object" || Array.isArray(report)) {
    fail(`${label} root must be an object`);
  }
  if (report.schemaVersion !== 1 || report.kind !== REPORT_KIND || report.status !== "passed") {
    fail(`${label} has an unsupported identity or unsuccessful status`);
  }
  if (!report.host || typeof report.host !== "object") {
    fail(`${label}.host must be an object`);
  }
  const os = safeName(report.host.os, `${label}.host.os`);
  const architecture = safeName(report.host.architecture, `${label}.host.architecture`);
  if (!report.config || typeof report.config !== "object") {
    fail(`${label}.config must be an object`);
  }
  const profile = safeName(report.config.profile, `${label}.config.profile`);
  const warmups = finiteNumber(report.config.warmups, `${label}.config.warmups`);
  const measuredRuns = finiteNumber(report.config.measuredRuns, `${label}.config.measuredRuns`, {
    positive: true,
  });
  if (!Array.isArray(report.toolchains) || report.toolchains.length > MAX_LANGUAGES) {
    fail(`${label}.toolchains has an invalid size`);
  }
  const toolchains = new Map();
  for (const [index, toolchain] of report.toolchains.entries()) {
    const language = safeName(toolchain?.language, `${label}.toolchains[${index}].language`);
    if (toolchains.has(language) || !SHA256.test(toolchain?.sourceSha256)) {
      fail(`${label} has a repeated toolchain or malformed source digest`);
    }
    toolchains.set(language, {
      compileMs: finiteNumber(toolchain.compileMs, `${label}.${language}.compileMs`, {
        positive: true,
      }),
      binaryBytes: finiteNumber(toolchain.binaryBytes, `${label}.${language}.binaryBytes`, {
        positive: true,
      }),
      sourceSha256: toolchain.sourceSha256.toLowerCase(),
    });
  }
  if (!Array.isArray(report.cases) || report.cases.length === 0 || report.cases.length > MAX_CASES) {
    fail(`${label}.cases has an invalid size`);
  }
  const cases = new Map();
  for (const [index, benchmarkCase] of report.cases.entries()) {
    const name = safeName(benchmarkCase?.name, `${label}.cases[${index}].name`);
    if (cases.has(name)) {
      fail(`${label} repeats case ${name}`);
    }
    finiteNumber(benchmarkCase.scale, `${label}.${name}.scale`);
    if (!Array.isArray(benchmarkCase.results) || benchmarkCase.results.length === 0 || benchmarkCase.results.length > MAX_LANGUAGES) {
      fail(`${label}.${name}.results has an invalid size`);
    }
    const results = new Map();
    for (const [resultIndex, result] of benchmarkCase.results.entries()) {
      const language = safeName(
        result?.language,
        `${label}.${name}.results[${resultIndex}].language`,
      );
      if (results.has(language)) {
        fail(`${label}.${name} repeats language ${language}`);
      }
      results.set(
        language,
        finiteNumber(result.medianMs, `${label}.${name}.${language}.medianMs`, { positive: true }),
      );
    }
    cases.set(name, { scale: benchmarkCase.scale, results });
  }
  return { os, architecture, profile, warmups, measuredRuns, cases, toolchains };
}

export function readReport(filename, label) {
  const stat = fs.lstatSync(filename);
  if (stat.isSymbolicLink() || !stat.isFile() || stat.size === 0 || stat.size > MAX_REPORT_BYTES) {
    fail(`${label} is not a bounded regular report file`);
  }
  let report;
  try {
    report = JSON.parse(fs.readFileSync(filename, "utf8"));
  } catch (error) {
    fail(`${label} is not valid JSON: ${error.message}`);
  }
  return validateReport(report, label);
}

function matchingRuntimeEntries(base, candidate) {
  const runtime = [];
  for (const [caseName, candidateCase] of candidate.cases) {
    const baseCase = base.cases.get(caseName);
    if (!baseCase || baseCase.scale !== candidateCase.scale) {
      fail(`candidate case ${caseName} does not match the base scale`);
    }
    for (const [language, candidateMedian] of candidateCase.results) {
      const baseMedian = baseCase.results.get(language);
      if (baseMedian === undefined) {
        fail(`candidate result ${caseName}/${language} has no base result`);
      }
      runtime.push({ caseName, language, base: baseMedian, candidate: candidateMedian });
    }
    if (candidateCase.results.size !== baseCase.results.size) {
      fail(`candidate case ${caseName} does not contain the same languages as base`);
    }
  }
  if (candidate.cases.size !== base.cases.size) {
    fail("candidate and base contain different case sets");
  }
  return runtime;
}

function deltaPercent(base, candidate) {
  return ((candidate / base) - 1) * 100;
}

function formatDelta(value) {
  return `${value > 0 ? "+" : ""}${value.toFixed(1)}%`;
}

export function renderComment(base, candidate, sha) {
  if (typeof sha !== "string" || !/^[0-9a-f]{7,64}$/i.test(sha)) {
    fail("head SHA is malformed");
  }
  if (
    base.os !== candidate.os ||
    base.architecture !== candidate.architecture ||
    base.profile !== candidate.profile ||
    base.warmups !== candidate.warmups ||
    base.measuredRuns !== candidate.measuredRuns
  ) {
    fail("base and candidate were not measured with the same host profile");
  }
  const runtime = matchingRuntimeEntries(base, candidate);
  const lines = [
    "## Loom benchmark comparison",
    "",
    `Candidate \`${sha.slice(0, 12)}\` · \`${candidate.os}/${candidate.architecture}\` · \`${candidate.profile}\` profile`,
    "",
    "> Base and candidate ran sequentially on the same shared GitHub runner. Deltas are diagnostic evidence, not a release gate or a general language ranking.",
    "",
    "### Runtime median",
    "",
    "| Case | Language | Base | Candidate | Delta |",
    "|---|---:|---:|---:|---:|",
  ];
  for (const result of runtime) {
    lines.push(
      `| \`${result.caseName}\` | ${result.language} | ${result.base.toFixed(3)} ms | ${result.candidate.toFixed(3)} ms | ${formatDelta(deltaPercent(result.base, result.candidate))} |`,
    );
  }
  lines.push(
    "",
    "<details>",
    "<summary>Cold-like compiler invocation and artifact size</summary>",
    "",
    "| Language | Base compile | Candidate compile | Delta | Base bytes | Candidate bytes |",
    "|---|---:|---:|---:|---:|---:|",
  );
  for (const [language, candidateToolchain] of candidate.toolchains) {
    const baseToolchain = base.toolchains.get(language);
    if (
      !baseToolchain ||
      baseToolchain.sourceSha256 !== candidateToolchain.sourceSha256
    ) {
      fail(`candidate toolchain ${language} has no comparable base source`);
    }
    lines.push(
      `| ${language} | ${baseToolchain.compileMs.toFixed(2)} ms | ${candidateToolchain.compileMs.toFixed(2)} ms | ${formatDelta(deltaPercent(baseToolchain.compileMs, candidateToolchain.compileMs))} | ${Math.round(baseToolchain.binaryBytes)} | ${Math.round(candidateToolchain.binaryBytes)} |`,
    );
  }
  if (candidate.toolchains.size !== base.toolchains.size) {
    fail("candidate and base contain different toolchain sets");
  }
  lines.push("", "</details>", "");
  return lines.join("\n");
}

function parseArguments(argv) {
  const values = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined || values.has(flag)) {
      fail("expected unique --base, --candidate, --sha and --output arguments");
    }
    values.set(flag, value);
  }
  for (const flag of ["--base", "--candidate", "--sha", "--output"]) {
    if (!values.has(flag)) {
      fail(`missing ${flag}`);
    }
  }
  if (values.size !== 4) {
    fail("unknown command-line argument");
  }
  return values;
}

function main() {
  const argumentsMap = parseArguments(process.argv.slice(2));
  const base = readReport(argumentsMap.get("--base"), "base");
  const candidate = readReport(argumentsMap.get("--candidate"), "candidate");
  const output = argumentsMap.get("--output");
  fs.mkdirSync(path.dirname(output), { recursive: true });
  fs.writeFileSync(output, `${renderComment(base, candidate, argumentsMap.get("--sha"))}\n`, {
    flag: "wx",
  });
}

const isMain = process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1]);
if (isMain) {
  main();
}

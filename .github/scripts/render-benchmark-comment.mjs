import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const MAX_REPORT_BYTES = 2 * 1024 * 1024;
const MAX_CASES = 32;
const MAX_LANGUAGES = 16;
const MAX_PLATFORMS = 4;
const REPORT_KIND = "loom-cross-language-basic-benchmark";
const SAFE_NAME = /^[A-Za-z0-9_.+()-]{1,64}$/;
const SAFE_ARTIFACT_SUFFIX = /^[A-Za-z0-9_.-]{1,64}$/;
const SHA256 = /^[0-9a-f]{64}$/i;
const PLATFORM_COLUMNS = [
  { key: "macos/aarch64", label: "macOS", unavailable: "— (not measured)" },
  { key: "linux/x86_64", label: "Linux", unavailable: "— (not measured)" },
  { key: "windows/x86_64", label: "Windows", unavailable: "— (frontend only)" },
];

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

function safeInteger(value, label, { positive = false } = {}) {
  if (!Number.isSafeInteger(value)) {
    fail(`${label} must be a safe integer`);
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
  const warmups = safeInteger(report.config.warmups, `${label}.config.warmups`);
  const measuredRuns = safeInteger(report.config.measuredRuns, `${label}.config.measuredRuns`, {
    positive: true,
  });
  if (
    !Array.isArray(report.toolchains) ||
    report.toolchains.length === 0 ||
    report.toolchains.length > MAX_LANGUAGES
  ) {
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
      binaryBytes: safeInteger(toolchain.binaryBytes, `${label}.${language}.binaryBytes`, {
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
    const scale = safeInteger(benchmarkCase.scale, `${label}.${name}.scale`, { positive: true });
    if (
      !Array.isArray(benchmarkCase.results) ||
      benchmarkCase.results.length === 0 ||
      benchmarkCase.results.length > MAX_LANGUAGES
    ) {
      fail(`${label}.${name}.results has an invalid size`);
    }
    const results = new Map();
    for (const [resultIndex, result] of benchmarkCase.results.entries()) {
      const language = safeName(
        result?.language,
        `${label}.${name}.results[${resultIndex}].language`,
      );
      if (results.has(language) || !toolchains.has(language)) {
        fail(`${label}.${name} has a repeated or unknown language ${language}`);
      }
      results.set(
        language,
        finiteNumber(result.medianMs, `${label}.${name}.${language}.medianMs`, { positive: true }),
      );
    }
    if (results.size !== toolchains.size) {
      fail(`${label}.${name} does not contain every toolchain`);
    }
    cases.set(name, { scale, results });
  }
  return { os, architecture, profile, warmups, measuredRuns, cases, toolchains };
}

export function readReport(filename, label) {
  let stat;
  try {
    stat = fs.lstatSync(filename);
  } catch {
    fail(`${label} does not exist`);
  }
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

function validateComparison(base, candidate, label) {
  if (
    base.os !== candidate.os ||
    base.architecture !== candidate.architecture ||
    base.profile !== candidate.profile ||
    base.warmups !== candidate.warmups ||
    base.measuredRuns !== candidate.measuredRuns
  ) {
    fail(`${label} base and candidate were not measured with the same host profile`);
  }

  const runtime = new Map();
  for (const [caseName, candidateCase] of candidate.cases) {
    const baseCase = base.cases.get(caseName);
    if (!baseCase || baseCase.scale !== candidateCase.scale) {
      fail(`${label} candidate case ${caseName} does not match the base scale`);
    }
    for (const [language, candidateMedian] of candidateCase.results) {
      const baseMedian = baseCase.results.get(language);
      if (baseMedian === undefined) {
        fail(`${label} candidate result ${caseName}/${language} has no base result`);
      }
      runtime.set(`${caseName}\0${language}`, {
        caseName,
        language,
        scale: candidateCase.scale,
        base: baseMedian,
        candidate: candidateMedian,
      });
    }
    if (candidateCase.results.size !== baseCase.results.size) {
      fail(`${label} candidate case ${caseName} does not contain the same languages as base`);
    }
  }
  if (candidate.cases.size !== base.cases.size) {
    fail(`${label} candidate and base contain different case sets`);
  }

  const tools = new Map();
  for (const [language, candidateToolchain] of candidate.toolchains) {
    const baseToolchain = base.toolchains.get(language);
    if (!baseToolchain || baseToolchain.sourceSha256 !== candidateToolchain.sourceSha256) {
      fail(`${label} candidate toolchain ${language} has no comparable base source`);
    }
    tools.set(language, { base: baseToolchain, candidate: candidateToolchain });
  }
  if (candidate.toolchains.size !== base.toolchains.size) {
    fail(`${label} candidate and base contain different toolchain sets`);
  }

  return {
    key: `${candidate.os}/${candidate.architecture}`,
    profile: candidate.profile,
    warmups: candidate.warmups,
    measuredRuns: candidate.measuredRuns,
    runtime,
    tools,
  };
}

export function readComparisons(evidenceDirectory, runId) {
  if (typeof runId !== "string" || !/^[1-9][0-9]{0,19}$/.test(runId)) {
    fail("workflow run ID is malformed");
  }
  let rootStat;
  try {
    rootStat = fs.lstatSync(evidenceDirectory);
  } catch {
    fail("evidence directory does not exist");
  }
  if (rootStat.isSymbolicLink() || !rootStat.isDirectory()) {
    fail("evidence root is not a regular directory");
  }

  const entries = fs.readdirSync(evidenceDirectory, { withFileTypes: true });
  if (entries.length === 0 || entries.length > MAX_PLATFORMS) {
    fail("evidence contains an invalid number of platform artifacts");
  }

  const prefix = `pr-benchmark-${runId}`;
  const comparisons = [];
  const platforms = new Set();
  for (const entry of entries) {
    const suffix = entry.name === prefix ? null : entry.name.slice(prefix.length + 1);
    if (
      entry.isSymbolicLink() ||
      !entry.isDirectory() ||
      (entry.name !== prefix &&
        (!entry.name.startsWith(`${prefix}-`) || !SAFE_ARTIFACT_SUFFIX.test(suffix)))
    ) {
      fail(`unexpected benchmark artifact directory ${entry.name}`);
    }
    const directory = path.join(evidenceDirectory, entry.name);
    const files = fs.readdirSync(directory, { withFileTypes: true });
    if (
      files.length !== 2 ||
      files.some((file) => file.isSymbolicLink() || !file.isFile()) ||
      files.map((file) => file.name).sort().join("\n") !==
        "benchmark-base.json\nbenchmark-candidate.json"
    ) {
      fail(`${entry.name} must contain exactly one base and one candidate report`);
    }
    const base = readReport(path.join(directory, "benchmark-base.json"), `${entry.name} base`);
    const candidate = readReport(
      path.join(directory, "benchmark-candidate.json"),
      `${entry.name} candidate`,
    );
    const comparison = validateComparison(base, candidate, entry.name);
    if (platforms.has(comparison.key)) {
      fail(`platform ${comparison.key} appears more than once`);
    }
    if (suffix !== null && suffix !== comparison.key.replace("/", "-")) {
      fail(`${entry.name} does not match report platform ${comparison.key}`);
    }
    platforms.add(comparison.key);
    comparisons.push(comparison);
  }
  return comparisons.sort((left, right) => left.key.localeCompare(right.key));
}

function deltaPercent(base, candidate) {
  return ((candidate / base) - 1) * 100;
}

function formatDelta(value) {
  return `${value > 0 ? "+" : ""}${value.toFixed(1)}%`;
}

function runtimeCell(entry, unavailable) {
  if (!entry) {
    return unavailable;
  }
  return `${entry.base.toFixed(3)} / ${entry.candidate.toFixed(3)} / ${formatDelta(deltaPercent(entry.base, entry.candidate))}`;
}

function compileCell(entry, unavailable) {
  if (!entry) {
    return unavailable;
  }
  return `${entry.base.compileMs.toFixed(2)} / ${entry.candidate.compileMs.toFixed(2)} / ${formatDelta(deltaPercent(entry.base.compileMs, entry.candidate.compileMs))}`;
}

function sizeCell(entry, unavailable) {
  if (!entry) {
    return unavailable;
  }
  return `${entry.base.binaryBytes} / ${entry.candidate.binaryBytes} / ${formatDelta(deltaPercent(entry.base.binaryBytes, entry.candidate.binaryBytes))}`;
}

function requireSameSuite(comparisons) {
  const first = comparisons[0];
  const runtimeKeys = [...first.runtime.keys()];
  const toolKeys = [...first.tools.keys()];
  for (const comparison of comparisons.slice(1)) {
    if (
      comparison.profile !== first.profile ||
      comparison.warmups !== first.warmups ||
      comparison.measuredRuns !== first.measuredRuns ||
      [...comparison.runtime.keys()].join("\n") !== runtimeKeys.join("\n") ||
      [...comparison.tools.keys()].join("\n") !== toolKeys.join("\n")
    ) {
      fail("platform reports do not describe the same benchmark suite and profile");
    }
    for (const key of runtimeKeys) {
      if (comparison.runtime.get(key).scale !== first.runtime.get(key).scale) {
        fail("platform reports do not use the same case scales");
      }
    }
  }
  return { first, runtimeKeys, toolKeys };
}

export function renderComment(comparisons, sha) {
  if (typeof sha !== "string" || !/^[0-9a-f]{7,64}$/i.test(sha)) {
    fail("head SHA is malformed");
  }
  if (!Array.isArray(comparisons) || comparisons.length === 0 || comparisons.length > MAX_PLATFORMS) {
    fail("comparison list has an invalid size");
  }
  const byPlatform = new Map();
  const supportedPlatforms = new Set(PLATFORM_COLUMNS.map((platform) => platform.key));
  for (const comparison of comparisons) {
    if (!supportedPlatforms.has(comparison.key)) {
      fail(`platform ${comparison.key} has no report column`);
    }
    if (byPlatform.has(comparison.key)) {
      fail(`platform ${comparison.key} appears more than once`);
    }
    byPlatform.set(comparison.key, comparison);
  }
  const { first, runtimeKeys, toolKeys } = requireSameSuite(comparisons);
  const lines = [
    "## Loom benchmark comparison",
    "",
    `Candidate \`${sha.slice(0, 12)}\` · \`${first.profile}\` profile · ${first.warmups} warmups · ${first.measuredRuns} measured runs`,
    "",
    "> Each cell is `base / candidate / delta`. Deltas compare revisions only within the same platform and shared runner; they are diagnostic evidence, not a release gate or a cross-platform language ranking.",
    "",
  ];
  if (!byPlatform.has("windows/x86_64")) {
    lines.push(
      "Windows remains a frontend-only CI target, so its native runtime cells are unavailable until the Windows backend, runtime, and I/O reactor are complete.",
      "",
    );
  }
  lines.push(
    "### Runtime median (ms)",
    "",
    `| Case | Language | ${PLATFORM_COLUMNS.map((platform) => `${platform.label} (base / candidate / Δ)`).join(" | ")} |`,
    `| --- | --- | ${PLATFORM_COLUMNS.map(() => "---:").join(" | ")} |`,
  );
  for (const key of runtimeKeys) {
    const exemplar = first.runtime.get(key);
    const cells = PLATFORM_COLUMNS.map((platform) =>
      runtimeCell(byPlatform.get(platform.key)?.runtime.get(key), platform.unavailable),
    );
    lines.push(`| \`${exemplar.caseName}\` | ${exemplar.language} | ${cells.join(" | ")} |`);
  }

  lines.push(
    "",
    "<details>",
    "<summary>Cold-like compiler invocation and artifact size</summary>",
    "",
    "#### Compile time (ms)",
    "",
    `| Language | ${PLATFORM_COLUMNS.map((platform) => `${platform.label} (base / candidate / Δ)`).join(" | ")} |`,
    `| --- | ${PLATFORM_COLUMNS.map(() => "---:").join(" | ")} |`,
  );
  for (const language of toolKeys) {
    const cells = PLATFORM_COLUMNS.map((platform) =>
      compileCell(byPlatform.get(platform.key)?.tools.get(language), platform.unavailable),
    );
    lines.push(`| ${language} | ${cells.join(" | ")} |`);
  }

  lines.push(
    "",
    "#### Artifact size (bytes)",
    "",
    `| Language | ${PLATFORM_COLUMNS.map((platform) => `${platform.label} (base / candidate / Δ)`).join(" | ")} |`,
    `| --- | ${PLATFORM_COLUMNS.map(() => "---:").join(" | ")} |`,
  );
  for (const language of toolKeys) {
    const cells = PLATFORM_COLUMNS.map((platform) =>
      sizeCell(byPlatform.get(platform.key)?.tools.get(language), platform.unavailable),
    );
    lines.push(`| ${language} | ${cells.join(" | ")} |`);
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
      fail("expected unique --evidence-dir, --run-id, --sha and --output arguments");
    }
    values.set(flag, value);
  }
  for (const flag of ["--evidence-dir", "--run-id", "--sha", "--output"]) {
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
  const comparisons = readComparisons(
    argumentsMap.get("--evidence-dir"),
    argumentsMap.get("--run-id"),
  );
  const output = argumentsMap.get("--output");
  fs.mkdirSync(path.dirname(output), { recursive: true });
  fs.writeFileSync(output, `${renderComment(comparisons, argumentsMap.get("--sha"))}\n`, {
    flag: "wx",
  });
}

const isMain = process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1]);
if (isMain) {
  main();
}

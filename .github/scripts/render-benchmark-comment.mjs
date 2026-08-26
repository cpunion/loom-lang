import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const MAX_REPORT_BYTES = 2 * 1024 * 1024;
const MAX_CASES = 32;
const MAX_LANGUAGES = 16;
const MAX_PLATFORMS = 4;
const MAX_CHART_ENTRIES = 64;
const MAX_CHART_INDEX = 10_000;
const REPORT_KIND = "loom-cross-language-basic-benchmark";
const SAFE_NAME = /^[A-Za-z0-9_.+()-]{1,64}$/;
const SAFE_ARTIFACT_SUFFIX = /^[A-Za-z0-9_.-]{1,64}$/;
const SHA256 = /^[0-9a-f]{64}$/i;
const PLATFORM_COLUMNS = [
  {
    key: "macos/aarch64",
    label: "macOS",
    unavailable: "— (not measured)",
    chartColor: "#0969da",
    chartLegend: "blue candidate bars",
  },
  {
    key: "linux/x86_64",
    label: "Linux",
    unavailable: "— (not measured)",
    chartColor: "#eb670f",
    chartLegend: "orange candidate bars",
  },
  { key: "windows/x86_64", label: "Windows", unavailable: "— (frontend only)" },
];
const RUNTIME_CHART_PLATFORMS = PLATFORM_COLUMNS.filter((platform) => platform.chartColor);
const BASE_CHART_COLOR = "#59636e";

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
  const delta = ((candidate / base) - 1) * 100;
  if (!Number.isFinite(delta)) {
    fail("comparison delta must be finite");
  }
  return delta;
}

function formatDelta(value) {
  return `${value > 0 ? "+" : ""}${value.toFixed(1)}%`;
}

function comparisonCell(base, candidate, unit, precision, unavailable) {
  if (base === undefined || candidate === undefined) {
    return unavailable;
  }
  return `${base.toFixed(precision)} ${unit} \\| ${candidate.toFixed(precision)} ${unit} \\| ${formatDelta(deltaPercent(base, candidate))}`;
}

function runtimeCell(entry, unavailable) {
  if (!entry) {
    return unavailable;
  }
  return comparisonCell(entry.base, entry.candidate, "ms", 3, unavailable);
}

function compileCell(entry, unavailable) {
  if (!entry) {
    return unavailable;
  }
  return comparisonCell(entry.base.compileMs, entry.candidate.compileMs, "ms", 2, unavailable);
}

function sizeCell(entry, unavailable) {
  if (!entry) {
    return unavailable;
  }
  return comparisonCell(entry.base.binaryBytes, entry.candidate.binaryBytes, "B", 0, unavailable);
}

function chartIndex(base, candidate) {
  const rounded = Number((deltaPercent(base, candidate) + 100).toFixed(1));
  const index = Object.is(rounded, -0) ? 0 : rounded;
  if (index > MAX_CHART_INDEX) {
    fail("runtime chart index exceeds the supported limit");
  }
  return index;
}

function chartUpperBound(values) {
  const maximum = Math.max(100, ...values);
  const padded = maximum + Math.max(5, (maximum - 100) * 0.1);
  const step = 10 ** Math.max(1, Math.floor(Math.log10(padded)) - 1);
  return Math.ceil(padded / step) * step;
}

function runtimeChartBlock(panel, runtimeKeys, upperBound) {
  const entries = runtimeKeys.map((key) => panel.comparison.runtime.get(key));
  const labels = entries.map((entry) => JSON.stringify(`${entry.caseName}/${entry.language}`));
  const baseLine = runtimeKeys.map(() => 100);
  const indices = runtimeKeys.map((key) => {
    const entry = panel.comparison.runtime.get(key);
    return chartIndex(entry.base, entry.candidate);
  });
  // Mermaid renders a one-value line as a zero-length, invisible path. Give a
  // single runtime entry a meaningful base -> candidate layout instead.
  if (runtimeKeys.length === 1) {
    labels.unshift(JSON.stringify("base"));
    baseLine.unshift(100);
    indices.unshift(100);
  }
  return [
    "```mermaid",
    "---",
    "config:",
    "  themeVariables:",
    "    xyChart:",
    `      plotColorPalette: "${panel.platform.chartColor}, ${BASE_CHART_COLOR}"`,
    "---",
    "xychart-beta horizontal",
    `  title "${panel.platform.label} runtime index (base = 100)"`,
    `  x-axis [${labels.join(", ")}]`,
    `  y-axis "Runtime index" 0 --> ${upperBound}`,
    `  bar [${indices.join(", ")}]`,
    `  line [${baseLine.join(", ")}]`,
    "```",
  ];
}

function runtimeCharts(byPlatform, runtimeKeys) {
  if (runtimeKeys.length > MAX_CHART_ENTRIES) {
    fail("benchmark suite contains too many runtime entries for a bounded chart");
  }
  const panels = RUNTIME_CHART_PLATFORMS.flatMap((platform) => {
    const comparison = byPlatform.get(platform.key);
    if (!comparison) {
      return [];
    }
    return [{ platform, comparison }];
  });
  if (panels.length === 0) {
    return null;
  }
  const upperBound = chartUpperBound(
    panels.flatMap((panel) =>
      runtimeKeys.map((key) => {
        const runtime = panel.comparison.runtime.get(key);
        return chartIndex(runtime.base, runtime.candidate);
      }),
    ),
  );
  const blocks = panels.map((panel) => runtimeChartBlock(panel, runtimeKeys, upperBound));
  const lines = blocks.flatMap((block, index) => (index === 0 ? block : ["", ...block]));
  lines.push("");
  return {
    description: `The platforms are intentionally separated into independent charts: ${panels
      .map((panel) => `${panel.platform.label} = ${panel.platform.chartLegend}`)
      .join("; ")}. Each index is computed only against the same-platform base, shown as a gray line at 100. The panels share one scale; lower is faster and higher is slower.`,
    lines,
  };
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
    "> Each cell is `base | candidate | delta`. Deltas compare revisions only within the same platform and shared runner; they are diagnostic evidence, not a release gate or a cross-platform language ranking.",
    "",
  ];
  if (!byPlatform.has("windows/x86_64")) {
    lines.push(
      "Windows remains a frontend-only CI target, so its native runtime cells are unavailable until the Windows backend, runtime, and I/O reactor are complete.",
      "",
    );
  }
  lines.push(
    `| Case | Language | ${PLATFORM_COLUMNS.map((platform) => `${platform.label} (base \\| candidate \\| delta)`).join(" | ")} |`,
    `| --- | --- | ${PLATFORM_COLUMNS.map(() => "---:").join(" | ")} |`,
  );
  for (const key of runtimeKeys) {
    const exemplar = first.runtime.get(key);
    const cells = PLATFORM_COLUMNS.map((platform) =>
      runtimeCell(byPlatform.get(platform.key)?.runtime.get(key), platform.unavailable),
    );
    lines.push(
      `| \`${exemplar.caseName}\` · runtime median | ${exemplar.language} | ${cells.join(" | ")} |`,
    );
  }

  for (const language of toolKeys) {
    const cells = PLATFORM_COLUMNS.map((platform) =>
      compileCell(byPlatform.get(platform.key)?.tools.get(language), platform.unavailable),
    );
    lines.push(`| Compiler invocation | ${language} | ${cells.join(" | ")} |`);
  }

  for (const language of toolKeys) {
    const cells = PLATFORM_COLUMNS.map((platform) =>
      sizeCell(byPlatform.get(platform.key)?.tools.get(language), platform.unavailable),
    );
    lines.push(`| Artifact size | ${language} | ${cells.join(" | ")} |`);
  }
  const chart = runtimeCharts(byPlatform, runtimeKeys);
  if (chart) {
    lines.push("", "### Runtime comparison charts", "", chart.description, "", ...chart.lines);
  }
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

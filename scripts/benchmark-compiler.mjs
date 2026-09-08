#!/usr/bin/env node
// Fresh compiler processes; optional paired native object-cache comparison.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { arch, cpus, platform, release } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
let compiler = join(root, "target/loom");
let reportPath = join(root, "target/performance/compiler.json");
let runs = 3;
let extended = false;
let compareCache = false;
let sizes = [10, 50, 200];
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 1) {
  const option = args[index];
  if (option === "--help" || option === "-h") {
    console.log(`Usage: node scripts/benchmark-compiler.mjs [options]
  --compiler path  Compiler executable (default: target/loom)
  --output path    JSON report (default: target/performance/compiler.json)
  --runs N         Fresh measured processes per case, 1..20 (default: 3)
  --extended       Add startup, test --no-run, and generated package growth
  --compare-cache  Pair native builds with and without a warm object cache
  --sizes N,N,...  Generated helper counts, 1..512 (default: 10,50,200);
                   implies --extended, at most eight distinct sizes

macOS only: one unmeasured warmup per case, warm OS caches. --compare-cache
also records an initial cache miss separately, then alternates measured order.
Both variants start fresh processes, check sources, and link current artifacts.
Generated packages exercise multiple files, an imported package, records,
generic calls and Lists. Their tests compile without running. Native phase
timings and OS peak RSS are reported separately. Temporary inputs are removed.`);
    process.exit(0);
  }
  if (option === "--extended") { extended = true; continue; }
  if (option === "--compare-cache") { compareCache = true; continue; }
  const value = args[++index];
  if (!value || value.startsWith("--")) throw new Error(`${option} needs a value`);
  if (option === "--compiler") compiler = resolve(value);
  else if (option === "--output") reportPath = resolve(value);
  else if (option === "--runs") runs = Number(value);
  else if (option === "--sizes") { sizes = value.split(",").map(Number); extended = true; }
  else throw new Error(`unknown option ${option}`);
}
if (!Number.isInteger(runs) || runs < 1 || runs > 20) throw new Error("--runs must be 1..20");
if (sizes.length > 8 || new Set(sizes).size !== sizes.length ||
    sizes.some(size => !Number.isInteger(size) || size < 1 || size > 512)) {
  throw new Error("--sizes needs one to eight distinct integers in 1..512");
}
if (platform() !== "darwin") throw new Error("this initial compiler benchmark uses macOS /usr/bin/time -l");

const environment = { ...process.env, LOOM_NATIVE_TIMINGS: "1" };
delete environment.LOOM_GC_STRESS;
mkdirSync(join(root, "target/performance"), { recursive: true });
const median = values => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const version = spawnSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" });
const dirty = spawnSync("git", ["status", "--porcelain"], { cwd: root, encoding: "utf8" });
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
function output(command, arguments_) {
  const result = spawnSync(command, arguments_, { cwd: root, env: environment, encoding: "utf8" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} failed:\n${result.stderr}`);
  return result.stdout.trim();
}
function inputs(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return entry.name === "target" ? [] : inputs(path);
    return entry.isFile() && (entry.name.endsWith(".loom") || entry.name === "loom.toml") ? [path] : [];
  });
}
const sourceFiles = ["compiler/loom", "compiler/std", "compiler/examples"].flatMap(path => inputs(join(root, path))).sort();
const report = {
  schema: 1,
  revision: version.stdout.trim(),
  dirty: dirty.stdout.trim() !== "",
  compiler,
  compilerVersion: output(compiler, ["--version"]),
  compilerSha256: hash(readFileSync(compiler)),
  harnessSha256: hash(readFileSync(fileURLToPath(import.meta.url))),
  sourceSha256: hash(JSON.stringify(sourceFiles.map(path => [relative(root, path), hash(readFileSync(path))]))),
  sourceFiles: sourceFiles.length,
  nativeToolSha256: hash(readFileSync(join(root, "target/debug/loom-native"))),
  runtimeArchiveSha256: hash(readFileSync(process.env.LOOM_RUNTIME_LIBRARY ?? join(root, "target/debug/libloom_runtime.a"))),
  nativeOptimization: process.env.LOOM_OPT_LEVEL ?? "2",
  extended,
  compareCache,
  generatedSizes: extended ? sizes : [],
  host: { os: platform(), release: release(), arch: arch(), cpu: cpus()[0]?.model },
  method: compareCache
    ? "fresh processes; one uncached warmup and one initial cache miss per native case; paired samples alternate uncached/cache order; sample indexes identify pairs; source checking and linking always run"
    : "fresh processes; one warmup; warmed OS caches; no incremental compiler cache",
  wall: "elapsed around /usr/bin/time launch, including process launch and output; startup runs --version without loading a package",
  rss: "maximum RSS reported by macOS time, not a sum of simultaneous process memory",
  cases: [],
};
const temporary = mkdtempSync(join(root, "target/performance/run-"));

function measure(mode, packagePath, cache = null, expectedCache = null) {
  const nativeMode = mode === "build" || mode === "test --no-run";
  const command = [compiler, ...mode.split(" ")];
  if (packagePath) command.push(resolve(root, packagePath),
    "--std", join(root, "compiler/std"), "--native-tool", join(root, "target/debug/loom-native"));
  if (nativeMode) command.push("--output", join(temporary, "program"));
  if (cache) command.push("--object-cache", cache);
  const started = process.hrtime.bigint();
  const result = spawnSync("/usr/bin/time", ["-l", ...command], {
    cwd: root, env: environment, encoding: "utf8", maxBuffer: 1024 * 1024,
  });
  const wallMs = Number(process.hrtime.bigint() - started) / 1e6;
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${mode} ${packagePath} failed:\n${result.stdout}${result.stderr}`);
  const rss = result.stderr.match(/(\d+)\s+maximum resident set size/);
  if (!rss) throw new Error("missing macOS peak RSS measurement");
  const phases = Object.fromEntries([...result.stderr.matchAll(/^loom-native timings: (.+)$/gm)]
    .flatMap(line => [...line[1].matchAll(/(decode|codegen|link)_ms=([\d.]+)/g)].map(field => [field[1], Number(field[2])])));
  if (expectedCache) {
    const markers = [...result.stderr.matchAll(/^loom cache: (.+)$/gm)].map(match => match[1]);
    if (markers.length !== 1 || markers[0] !== expectedCache) throw new Error(`expected cache ${expectedCache}:\n${result.stderr}`);
    if (expectedCache === "hit" && (phases.decode !== undefined || phases.codegen !== undefined ||
        result.stderr.includes("loom-native phase: codegen"))) throw new Error("cache hit unexpectedly decoded or generated native code");
  }
  if (nativeMode && (!Number.isFinite(phases.link) || (expectedCache !== "hit" &&
      (!Number.isFinite(phases.decode) || !Number.isFinite(phases.codegen))))) {
    throw new Error("missing native phase timings; rebuild loom-native and ensure the package has tests");
  }
  return {
    wallMs, peakRssBytes: Number(rss[1]),
    ...(nativeMode ? { decodeMs: phases.decode ?? 0, codegenMs: phases.codegen ?? 0, linkMs: phases.link } : {}),
  };
}

function generated(size) {
  const directory = join(temporary, `growth-${size}`);
  mkdirSync(join(directory, "data"), { recursive: true });
  writeFileSync(join(directory, "loom.toml"), '[module]\nname = "benchmark"\n');
  writeFileSync(join(directory, "data/data.loom"),
    "pub record Item { amount Int label Text }\npub fn identity[T](value T) T { value }\n");
  const calls = Array.from({ length: size }, (_, index) => `    total = step_${index}(total)`);
  writeFileSync(join(directory, "main.loom"), `import benchmark.data.Item
import benchmark.data.identity
import std.list.new
import std.list.get
import std.list.push
import std.list.length
import std.process.arguments

fn main() {
    var total = length(arguments())
${calls.join("\n")}
    assert total >= ${size * (size + 1) / 2}
}
`);
  // Ten helpers per file grows both declaration count and package-wide lookup.
  for (let start = 0; start < size; start += 10) {
    const functions = [];
    for (let index = start; index < Math.min(start + 10, size); index += 1) {
      functions.push(`fn step_${index}(value Int) Int {
    let item = identity(Item { amount = value label = "item-${index}" })
    let values = new[Int]()
    push(values, item.amount)
    get(values, 0) + ${index + 1}
}`);
    }
    writeFileSync(join(directory, `helpers_${start}.loom`), `${functions.join("\n\n")}\n`);
  }
  writeFileSync(join(directory, "main_test.loom"), `test fn generated_helpers() {
${Array.from({ length: size }, (_, index) => `    assert step_${index}(0) == ${index + 1}`).join("\n")}
}
`);
  const files = inputs(directory).sort();
  return {
    name: `growth-${size}`, package: relative(root, directory),
    generated: {
      helpers: size, files: files.length,
      sourceBytes: files.reduce((total, path) => total + readFileSync(path).length, 0),
      sourceSha256: hash(JSON.stringify(files.map(path => [relative(directory, path), hash(readFileSync(path))]))),
    },
  };
}

function benchmark(name, mode, packagePath, generated) {
  measure(mode, packagePath);
  const display = (summary, variant = "—") => {
    const number = value => value === undefined ? "—" : value.toFixed(2);
    console.log(`| ${name} | ${mode} |${compareCache ? ` ${variant} |` : ""} ${number(summary.wallMs)} | ${number(summary.peakRssBytes / 1048576)} | ${number(summary.decodeMs)} | ${number(summary.codegenMs)} | ${number(summary.linkMs)} |`);
  };
  const record = (samples, extra = {}) => {
    const summary = Object.fromEntries(Object.keys(samples[0]).map(key => [key, median(samples.map(sample => sample[key]))]));
    report.cases.push({ name, command: mode, package: packagePath, ...(generated ? { generated } : {}), ...extra, median: summary, samples });
    display(summary, extra.variant);
  };
  if (!compareCache || (mode !== "build" && mode !== "test --no-run")) {
    record(Array.from({ length: runs }, () => measure(mode, packagePath)));
    return;
  }
  const cache = join(temporary, `cache-${report.cases.length}`);
  const firstMiss = measure(mode, packagePath, cache, "miss");
  display(firstMiss, "initial miss (observation)");
  const samples = [[], []];
  const positions = [[], []];
  for (let pair = 0; pair < runs; pair += 1) {
    const order = pair % 2 === 0 ? [0, 1] : [1, 0];
    for (const [position, variant] of order.entries()) {
      samples[variant].push(measure(mode, packagePath, variant ? cache : null, variant ? "hit" : null));
      positions[variant].push(position);
    }
  }
  record(samples[0], { variant: "uncached", flags: [], samplePositions: positions[0] });
  record(samples[1], { variant: "warm-object-cache", flags: ["--object-cache", cache], samplePositions: positions[1], firstMiss });
}

try {
  console.log(`| Case | Command |${compareCache ? " Variant |" : ""} Wall ms | RSS MiB | Decode ms | Codegen ms | Link ms |`);
  console.log(`| --- | --- |${compareCache ? " --- |" : ""} ---: | ---: | ---: | ---: | ---: |`);
  if (extended) benchmark("startup", "--version", null);
  const packages = [
    ["scalar", "compiler/examples/scalar"], ["data", "compiler/examples/data"], ["compiler", "compiler/loom"],
  ].map(([name, packagePath]) => ({ name, package: packagePath }));
  if (extended) packages.push(...sizes.map(generated));
  for (const item of packages) {
    for (const mode of extended ? ["check", "build", "test --no-run"] : ["check", "build"]) {
      benchmark(item.name, mode, item.package, item.generated);
    }
  }
  mkdirSync(dirname(reportPath), { recursive: true });
  writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  console.log(`\nRaw measurements: ${reportPath}`);
} finally {
  rmSync(temporary, { recursive: true });
}

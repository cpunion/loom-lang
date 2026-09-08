#!/usr/bin/env node
// Paired macOS CPU/RSS measurements, including toolchain and input provenance.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { arch, cpus, platform, release } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const codegen = args.at(-1) === "--codegen";
if (codegen) args.pop();
if (args.length !== 3 || args.includes("--help")) {
  console.log("Usage: node scripts/benchmark-gc.mjs BASELINE_TOOLCHAIN INPUT_ROOT OUTPUT_JSON [--codegen]\n"
    + "macOS only; baseline directory contains loom, loom-native, libloom_runtime.a.\n"
    + "Candidate uses target/loom and target/debug. INPUT_ROOT contains fixed compiler/loom and compiler/std sources.\n"
    + "Default: ten alternating pairs of lifetime kernels and compiler check; GC stress disabled.\n"
    + "--codegen instead measures three native-backend pairs on one fixed checked artifact.");
  process.exit(args.includes("--help") ? 0 : 1);
}
if (platform() !== "darwin") throw new Error("RSS units and time output require macOS");
const [baseline, input, output] = args.map(path => resolve(path));
const directory = join(root, "target/performance/gc-lifetimes");
mkdirSync(directory, { recursive: true });
mkdirSync(dirname(output), { recursive: true });
const env = { ...process.env };
delete env.LOOM_GC_STRESS;
delete env.LOOM_NATIVE_TIMINGS;
delete env.LOOM_RUNTIME_LIBRARY;
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
const file = path => ({ path, sha256: hash(readFileSync(path)) });
function execute(command, arguments_, extra = {}) {
  const start = process.hrtime.bigint();
  const result = spawnSync(command, arguments_, {
    cwd: root, env: { ...env, ...extra }, encoding: "utf8", maxBuffer: 4 * 1024 * 1024,
  });
  if (result.error || result.status !== 0) throw new Error(result.error?.message ?? result.stderr + result.stdout);
  return { ...result, wallMs: Number(process.hrtime.bigint() - start) / 1e6 };
}
function inputs(directory) {
  return readdirSync(directory, { withFileTypes: true }).flatMap(entry => {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) return entry.name === "target" ? [] : inputs(path);
    return entry.name.endsWith(".loom") || entry.name === "loom.toml"
      ? [[relative(input, path), hash(readFileSync(path))]] : [];
  });
}
const sourceInputs = [...inputs(join(input, "compiler/loom")), ...inputs(join(input, "compiler/std"))].sort();
const toolchains = {
  baseline: { compiler: file(join(baseline, "loom")), native: file(join(baseline, "loom-native")), runtime: file(join(baseline, "libloom_runtime.a")) },
  candidate: { compiler: file(join(root, "target/loom")), native: file(join(root, "target/debug/loom-native")), runtime: file(join(root, "target/debug/libloom_runtime.a")) },
};
for (const [key, tools] of Object.entries(toolchains)) {
  const binary = join(directory, key);
  execute(tools.compiler.path, ["build", join(root, "benchmarks/gc-lifetimes"),
    "--std", join(input, "compiler/std"), "--native-tool", tools.native.path, "--output", binary],
  { LOOM_RUNTIME_LIBRARY: tools.runtime.path, LOOM_OPT_LEVEL: "3" });
  tools.binary = file(binary);
  // A small run checks actual relocation paths without the full-size stress cost.
  const smoke = execute(binary, ["32"], { LOOM_GC_STRESS: "1" });
  if (smoke.stdout !== "248" || smoke.stderr !== "") throw new Error(`${key} GC stress checksum failed`);
}
const median = values => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
function measure(command, arguments_, expected) {
  const result = execute("/usr/bin/time", ["-l", command, ...arguments_],
    codegen ? { LOOM_NATIVE_TIMINGS: "1", LOOM_OPT_LEVEL: "2" } : {});
  if (result.stdout !== expected) throw new Error(`unexpected output: ${result.stdout}`);
  const rss = result.stderr.match(/(\d+)\s+maximum resident set size/);
  const cpu = result.stderr.match(/([\d.]+)\s+real\s+([\d.]+)\s+user\s+([\d.]+)\s+sys/);
  if (!rss || !cpu) throw new Error(result.stderr);
  const phases = result.stderr.match(/decode_ms=([\d.]+) codegen_ms=([\d.]+) link_ms=([\d.]+)/);
  if (codegen && !phases) throw new Error(result.stderr);
  return { wallMs: result.wallMs, cpuMs: (Number(cpu[2]) + Number(cpu[3])) * 1000, peakRssBytes: Number(rss[1]),
    ...(phases ? { decodeMs: Number(phases[1]), codegenMs: Number(phases[2]), linkMs: Number(phases[3]) } : {}) };
}
const checked = join(directory, "compiler.checked");
if (codegen) {
  writeFileSync(checked, execute(toolchains.candidate.compiler.path, ["emit-checked",
    join(input, "compiler/loom"), "--std", join(input, "compiler/std")]).stdout);
}
const workloads = codegen ? [
  { name: "compiler_codegen_o2", expected: "", command: tools => tools.native.path,
    args: tools => [checked, "--output", join(directory, "compiler-output"), "--runtime", tools.runtime.path] },
] : [
  ...[1048576, 8388608].map(count => ({
    name: "eight_disjoint_lists", count, expected: String(8 * (count - 1)),
    command: tools => tools.binary.path, args: [String(count)],
  })),
  { name: "compiler_check", expected: "checked package\n",
    command: tools => tools.compiler.path,
    args: ["check", join(input, "compiler/loom"), "--std", join(input, "compiler/std")] },
];
const report = {
  at: new Date().toISOString(),
  host: { os: platform(), release: release(), arch: arch(), cpu: cpus()[0]?.model },
  method: "Alternating pairs after one warmup each; fresh processes, warm OS caches, no incremental cache. Wall includes launch. CPU rounded by macOS time. Native codegen includes LLVM lowering/optimization/object emission, not frontend checking or linking.",
  runs: codegen ? 3 : 10,
  revision: execute("git", ["rev-parse", "HEAD"]).stdout.trim(),
  dirty: execute("git", ["status", "--porcelain"]).stdout !== "",
  cc: execute(env.LOOM_CC ?? "clang", ["--version"]).stdout.trim(),
  source: file(join(root, "benchmarks/gc-lifetimes/main.loom")),
  input, sourceFiles: sourceInputs.length, inputsSha256: hash(JSON.stringify(sourceInputs)),
  toolchains, ...(codegen ? { checked: file(checked) } : {}), cases: [],
};
for (const workload of workloads) {
  const samples = { baseline: [], candidate: [] };
  const run = key => measure(workload.command(toolchains[key]),
    typeof workload.args === "function" ? workload.args(toolchains[key]) : workload.args, workload.expected);
  for (const key of Object.keys(toolchains)) run(key);
  for (let round = 0; round < report.runs; round += 1) {
    for (const key of round % 2 ? ["candidate", "baseline"] : ["baseline", "candidate"]) samples[key].push(run(key));
  }
  const summary = Object.fromEntries(Object.entries(samples).map(([key, values]) => [key,
    Object.fromEntries(Object.keys(values[0]).map(field => {
      const center = median(values.map(value => value[field]));
      return [field, { median: center, mad: median(values.map(value => Math.abs(value[field] - center))) }];
    })),
  ]));
  report.cases.push({ name: workload.name, ...(workload.count ? { count: workload.count } : {}), expected: workload.expected, summary, samples });
  console.log(workload.name, JSON.stringify(summary));
}
writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);

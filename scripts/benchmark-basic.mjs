#!/usr/bin/env node
// Native executables, fresh processes, checked outputs, interleaved samples.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { arch, cpus, platform, release } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
let runs = 7;
let quick = false;
let reuse = false;
let output = join(root, "target/performance/basic.json");
const compiler = resolve(process.env.BENCH_LOOM ?? join(root, "target/loom"));
const cc = process.env.BENCH_CC ?? process.env.LOOM_CC ?? "clang";
const rustc = process.env.BENCH_RUSTC ?? "rustc";
const go = process.env.BENCH_GO ?? "go";
const zig = process.env.BENCH_ZIG ?? "zig";
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 1) {
  const arg = args[index];
  if (arg === "--quick") quick = true;
  else if (arg === "--reuse-build") reuse = true;
  else if (arg === "--runs") runs = Number(args[++index]);
  else if (arg === "--output") output = resolve(args[++index]);
  else if (arg === "--help") {
    console.log(`Usage: node scripts/benchmark-basic.mjs [--runs 7] [--quick] [--reuse-build] [--output path]
Tools: BENCH_LOOM, BENCH_CC, BENCH_RUSTC, BENCH_GO, BENCH_ZIG.
Native runtime comparison, not interpreter or compiler throughput. --quick only checks the harness.
--reuse-build reruns existing binaries; provenance is retained from their build manifest.`);
    process.exit(0);
  } else throw new Error(`unknown option: ${arg}`);
}
if (!Number.isInteger(runs) || runs < 1 || runs > 30) throw new Error("--runs must be 1..30");
const directory = join(root, "target/performance/basic");
mkdirSync(directory, { recursive: true });
mkdirSync(dirname(output), { recursive: true });
const env = { ...process.env };
delete env.LOOM_GC_STRESS;
delete env.LOOM_NATIVE_TIMINGS;
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
function execute(command, arguments_, extra = {}) {
  const start = process.hrtime.bigint();
  const result = spawnSync(command, arguments_, { cwd: root, env: { ...env, ...extra }, encoding: "utf8", maxBuffer: 1024 * 1024 });
  const wallMs = Number(process.hrtime.bigint() - start) / 1e6;
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} ${arguments_.join(" ")} failed: ${result.stderr}${result.stdout}`);
  return { wallMs, stdout: result.stdout.trim(), stderr: result.stderr.trim() };
}
const median = values => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const source = extension => join(root, `benchmarks/basic/main.${extension}`);
const binary = id => join(directory, id + (platform() === "win32" ? ".exe" : ""));
const cFlags = ["-std=c17", "-O3", ...(arch() === "arm64" ? ["-mcpu=native"] : ["-march=native"])];
const rustFlags = ["--edition", "2024", "-C", "opt-level=3", "-C", "target-cpu=native", "-C", "panic=abort"];
const variants = [
  { id: "loom-o3", label: "Loom O3", tool: compiler, args: ["build", dirname(source("loom")), "--output", binary("loom-o3")], env: { LOOM_OPT_LEVEL: "3" } },
  { id: "c-o3", label: "C O3", tool: cc, args: [...cFlags, source("c"), "-o", binary("c-o3")] },
  { id: "go", label: "Go", tool: go, args: ["build", "-trimpath", "-o", binary("go"), source("go")] },
  { id: "rust-o3", label: "Rust O3", tool: rustc, args: [...rustFlags, "-C", "overflow-checks=off", source("rs"), "-o", binary("rust-o3")] },
  { id: "zig-fast", label: "Zig Fast", tool: zig, args: ["build-exe", source("zig"), "-O", "ReleaseFast", "-mcpu=native", "-lc", `-femit-bin=${binary("zig-fast")}`] },
  { id: "loom-o2", label: "Loom O2", tool: compiler, args: ["build", dirname(source("loom")), "--output", binary("loom-o2")], env: { LOOM_OPT_LEVEL: "2" } },
  { id: "c-checked", label: "C checked", tool: cc, args: [...cFlags, "-ftrapv", source("c"), "-o", binary("c-checked")] },
  { id: "rust-checked", label: "Rust checked", tool: rustc, args: [...rustFlags, "-C", "overflow-checks=on", source("rs"), "-o", binary("rust-checked")] },
  { id: "zig-safe", label: "Zig Safe", tool: zig, args: ["build-exe", source("zig"), "-O", "ReleaseSafe", "-mcpu=native", "-lc", `-femit-bin=${binary("zig-safe")}`] },
];
const manifestPath = join(directory, "build.json");
let build;
if (reuse) {
  build = JSON.parse(readFileSync(manifestPath, "utf8"));
  for (const variant of variants) {
    if (hash(readFileSync(binary(variant.id))) !== build.variants.find(item => item.id === variant.id)?.sha256) {
      throw new Error(`${variant.id} no longer matches its build manifest; rebuild`);
    }
  }
} else {
  build = {
    revision: execute("git", ["rev-parse", "HEAD"]).stdout,
    dirty: execute("git", ["status", "--porcelain"]).stdout !== "",
    sources: Object.fromEntries(["loom", "c", "go", "rs", "zig"].map(ext => [ext, hash(readFileSync(source(ext)))])),
    loomCompilerSha256: hash(readFileSync(compiler)),
    nativeToolSha256: hash(readFileSync(join(root, "target/debug/loom-native"))),
    runtimeSha256: hash(readFileSync(process.env.LOOM_RUNTIME_LIBRARY ?? join(root, "target/debug", platform() === "win32" ? "loom_runtime.lib" : "libloom_runtime.a"))),
    tools: {
      loom: execute(compiler, ["--version"]).stdout,
      c: execute(cc, ["--version"]).stdout,
      go: execute(go, ["version"]).stdout,
      rust: execute(rustc, ["--version", "--verbose"]).stdout,
      zig: execute(zig, ["version"]).stdout,
    },
    variants: [],
  };
  for (const variant of variants) {
    process.stderr.write(`Building ${variant.label}...\n`);
    const result = execute(variant.tool, variant.args, variant.env);
    build.variants.push({ ...variant, buildWallMs: result.wallMs, binary: binary(variant.id), sha256: hash(readFileSync(binary(variant.id))) });
  }
  writeFileSync(manifestPath, `${JSON.stringify(build, null, 2)}\n`);
}

const cases = [
  { name: "startup", kernel: "int_lcg", count: 0, seed: 17 },
  { name: "int_lcg", count: quick ? 10000 : 20000000, seed: 17 },
  { name: "fib_recursive", count: quick ? 20 : 38, seed: 17 },
  { name: "record_value", count: quick ? 10000 : 15000000, seed: 17 },
  { name: "list_build_scan", count: quick ? 10000 : 12000000, seed: 17 },
  { name: "function_value", count: quick ? 10000 : 20000000, seed: 17 },
];
// Independent bounded-integer reference; all intermediates fit exact JS integers.
function reference(kernel, n, seed) {
  if (kernel === "fib_recursive") {
    let a = 0, b = 1;
    for (let i = 0; i < n; i += 1) [a, b] = [b, a + b];
    return a;
  }
  if (kernel === "record_value") {
    let x = seed % 97, y = seed % 193, z = seed % 389;
    for (let i = 0; i < n; i += 1) [x, y, z] = [(y + i % 31) % 1000003, (z + x) % 1000003, (x + y + z) % 1000003];
    return x + y + z;
  }
  let state = seed % 1000003, sum = 0;
  for (let i = 0; i < n; i += 1) {
    state = kernel === "function_value" && seed % 2 !== 0 ? (state * 7 + 11) % 1000003 : (state * 17 + 23) % 1000003;
    sum += state;
  }
  return kernel === "list_build_scan" ? sum : state;
}
const report = {
  schema: 1, at: new Date().toISOString(), host: { os: platform(), release: release(), arch: arch(), cpu: cpus()[0]?.model },
  method: "fresh native processes; one warmup per case/variant; rotated variant order per measured round; median and MAD; includes startup, argv parsing, allocation, stdout and process exit; no baseline subtraction",
  buildMethod: "one observed build invocation per variant, with existing tool caches; not comparable compiler-throughput measurements",
  quick, runs, build, cases: [],
};
for (const item of cases) {
  const kernel = item.kernel ?? item.name;
  const expected = String(reference(kernel, item.count, item.seed));
  const samples = new Map(variants.map(variant => [variant.id, []]));
  const measure = variant => {
    const result = execute(binary(variant.id), [kernel, String(item.count), String(item.seed)]);
    if (result.stdout !== expected || result.stderr !== "") throw new Error(`${variant.label}/${item.name} checksum mismatch: ${result.stdout} expected ${expected}; ${result.stderr}`);
    return result.wallMs;
  };
  for (const variant of variants) measure(variant);
  for (let round = 0; round < runs; round += 1) {
    for (let position = 0; position < variants.length; position += 1) {
      const variant = variants[(position + round) % variants.length];
      samples.get(variant.id).push(measure(variant));
    }
  }
  const results = variants.map(variant => {
    const values = samples.get(variant.id);
    const center = median(values);
    return { id: variant.id, label: variant.label, medianMs: center, madMs: median(values.map(value => Math.abs(value - center))), samplesMs: values };
  });
  report.cases.push({ ...item, expected, results });
  process.stderr.write(`Measured ${item.name}; checksum ${expected}\n`);
}
writeFileSync(output, `${JSON.stringify(report, null, 2)}\n`);
console.log(`| Case | ${variants.map(variant => variant.label).join(" | ")} |`);
console.log(`| --- | ${variants.map(() => "---:").join(" | ")} |`);
for (const item of report.cases) console.log(`| ${item.name} | ${item.results.map(value => value.medianMs.toFixed(3)).join(" | ")} |`);
console.log(`\nMilliseconds, median of ${runs}; report: ${output}${quick ? " (quick harness check, not a performance result)" : ""}`);

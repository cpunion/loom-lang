#!/usr/bin/env node
// Fresh compiler processes with one OS-cache warmup, not an incremental build.
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
const args = process.argv.slice(2);
for (let index = 0; index < args.length; index += 2) {
  const [option, value] = args.slice(index, index + 2);
  if (!value) throw new Error(`${option} needs a value`);
  if (option === "--compiler") compiler = resolve(value);
  else if (option === "--output") reportPath = resolve(value);
  else if (option === "--runs") runs = Number(value);
  else throw new Error(`unknown option ${option}`);
}
if (!Number.isInteger(runs) || runs < 1 || runs > 20) throw new Error("--runs must be 1..20");
if (platform() !== "darwin") throw new Error("this initial compiler benchmark uses macOS /usr/bin/time -l");

const environment = { ...process.env, LOOM_NATIVE_TIMINGS: "1" };
delete environment.LOOM_GC_STRESS;
mkdirSync(join(root, "target/performance"), { recursive: true });
const temporary = mkdtempSync(join(root, "target/performance/run-"));
const median = values => {
  const sorted = [...values].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2;
};
const version = spawnSync("git", ["rev-parse", "HEAD"], { cwd: root, encoding: "utf8" });
const dirty = spawnSync("git", ["status", "--porcelain"], { cwd: root, encoding: "utf8" });
const hash = bytes => createHash("sha256").update(bytes).digest("hex");
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
  compilerSha256: hash(readFileSync(compiler)),
  sourceSha256: hash(JSON.stringify(sourceFiles.map(path => [relative(root, path), hash(readFileSync(path))]))),
  sourceFiles: sourceFiles.length,
  nativeToolSha256: hash(readFileSync(join(root, "target/debug/loom-native"))),
  runtimeArchiveSha256: hash(readFileSync(process.env.LOOM_RUNTIME_LIBRARY ?? join(root, "target/debug/libloom_runtime.a"))),
  nativeOptimization: process.env.LOOM_OPT_LEVEL ?? "2",
  host: { os: platform(), release: release(), arch: arch(), cpu: cpus()[0]?.model },
  method: "fresh processes; one warmup; warmed OS caches; no incremental compiler cache",
  rss: "maximum RSS reported by macOS time, not a sum of simultaneous process memory",
  cases: [],
};

function measure(mode, packagePath) {
  const command = [compiler, mode, join(root, packagePath),
    "--std", join(root, "compiler/std"), "--native-tool", join(root, "target/debug/loom-native")];
  if (mode === "build") command.push("--output", join(temporary, "program"));
  const started = process.hrtime.bigint();
  const result = spawnSync("/usr/bin/time", ["-l", ...command], {
    cwd: root, env: environment, encoding: "utf8", maxBuffer: 1024 * 1024,
  });
  const wallMs = Number(process.hrtime.bigint() - started) / 1e6;
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${mode} ${packagePath} failed:\n${result.stdout}${result.stderr}`);
  const rss = result.stderr.match(/(\d+)\s+maximum resident set size/);
  if (!rss) throw new Error("missing macOS peak RSS measurement");
  const native = result.stderr.match(/loom-native timings: decode_ms=([\d.]+) llvm_ms=([\d.]+) link_ms=([\d.]+)/);
  if (mode === "build" && !native) throw new Error("rebuild loom-native to enable phase timings");
  return {
    wallMs, peakRssBytes: Number(rss[1]),
    ...(native ? { decodeMs: Number(native[1]), llvmMs: Number(native[2]), linkMs: Number(native[3]) } : {}),
  };
}

try {
  console.log("| Case | Command | Wall ms | RSS MiB | Decode ms | LLVM ms | Link ms |");
  console.log("| --- | --- | ---: | ---: | ---: | ---: | ---: |");
  for (const [name, packagePath] of [
    ["scalar", "compiler/examples/scalar"], ["data", "compiler/examples/data"], ["compiler", "compiler/loom"],
  ]) {
    for (const mode of ["check", "build"]) {
      measure(mode, packagePath);
      const samples = Array.from({ length: runs }, () => measure(mode, packagePath));
      const summary = Object.fromEntries(Object.keys(samples[0]).map(key => [key, median(samples.map(sample => sample[key]))]));
      report.cases.push({ name, command: mode, package: packagePath, median: summary, samples });
      const number = value => value === undefined ? "—" : value.toFixed(2);
      console.log(`| ${name} | ${mode} | ${number(summary.wallMs)} | ${number(summary.peakRssBytes / 1048576)} | ${number(summary.decodeMs)} | ${number(summary.llvmMs)} | ${number(summary.linkMs)} |`);
    }
  }
  mkdirSync(dirname(reportPath), { recursive: true });
  writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  console.log(`\nRaw measurements: ${reportPath}`);
} finally {
  rmSync(temporary, { recursive: true });
}

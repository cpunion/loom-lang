#!/usr/bin/env node
// Stage and exercise a local toolchain; this is not a standalone release packager.
import { spawnSync } from "node:child_process";
import { copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
if (args.length !== 1 || args[0].startsWith("-")) {
  console.error("Usage: node scripts/stage-toolchain.mjs <new-directory>");
  process.exit(args[0] === "--help" ? 0 : 2);
}
const output = resolve(args[0]);
if (existsSync(output)) throw new Error("toolchain destination already exists");
const suffix = process.platform === "win32" ? ".exe" : "";
const runtimeName = process.platform === "win32" ? "loom_runtime.lib" : "libloom_runtime.a";
const compiler = join(root, `target/loom${suffix}`);
const native = join(root, `target/debug/loom-native${suffix}`);
const runtime = join(root, `target/debug/${runtimeName}`);
for (const input of [compiler, native, runtime]) {
  if (!existsSync(input)) throw new Error(`missing ${input}; bootstrap this checkout first`);
}

function copyTree(from, to) {
  cpSync(from, to, {
    recursive: true,
    filter: path => !relative(from, path).split(sep).includes("target"),
  });
}
function run(executable, arguments_, cwd, environment = process.env) {
  const command = [executable, ...arguments_].map(value => JSON.stringify(value)).join(" ");
  writeSync(1, `Running ${command}\n`);
  const outcome = spawnSync(executable, arguments_, {
    cwd, env: environment, encoding: "utf8", maxBuffer: 4 * 1024 * 1024,
  });
  if (outcome.error) throw new Error(`cannot start ${command}`, { cause: outcome.error });
  writeSync(1, `Exit ${outcome.status}; signal ${outcome.signal ?? "none"}\n`);
  if (outcome.status !== 0) throw new Error(`${command} failed:\n${outcome.stdout}${outcome.stderr}`);
  return outcome.stdout;
}

// Keep the verification outside every checkout ancestor. No sidecar or cwd
// fallback can substitute the development compiler, source std, or runtime.
const temporary = mkdtempSync(join(tmpdir(), "loom-toolchain-"));
try {
  let prefix = join(temporary, "initial");
  const privateTools = join(prefix, "lib/loom");
  mkdirSync(join(prefix, "bin"), { recursive: true });
  mkdirSync(privateTools, { recursive: true });
  copyFileSync(native, join(privateTools, `loom-native${suffix}`));
  copyFileSync(runtime, join(privateTools, runtimeName));
  copyTree(join(root, "compiler/std"), join(privateTools, "std"));
  copyFileSync(join(root, "LICENSE"), join(prefix, "LICENSE"));
  const buildEnvironment = { ...process.env, LOOM_TARGET_CPU: "generic", LOOM_OPT_LEVEL: "2" };
  delete buildEnvironment.LOOM_GC_STRESS;
  // The current bridge locates its adjacent runtime, including after relocation.
  delete buildEnvironment.LOOM_RUNTIME_LIBRARY;
  console.log("Building the generic-CPU source compiler...");
  run(compiler, ["build", join(root, "compiler/loom"), "--std", join(privateTools, "std"),
    "--native-tool", join(privateTools, `loom-native${suffix}`),
    "--output", join(prefix, `bin/loom${suffix}`)], root, buildEnvironment);
  const moved = join(temporary, "moved 雪 toolchain");
  renameSync(prefix, moved);
  prefix = moved;
  const loom = join(prefix, `bin/loom${suffix}`);
  const app = join(temporary, "application");
  copyTree(join(root, "compiler/examples/wordcount"), app);
  const environment = { ...buildEnvironment };
  delete environment.LOOM_TARGET_CPU;
  // These native commands deliberately pass no --std or --native-tool flags.
  console.log("Checking the relocated compiler, source library and native tools...");
  run(loom, ["fmt", "--check", "--recursive", "."], app, environment);
  run(loom, ["check"], app, environment);
  run(loom, ["build", "--output", join(app, `wordcount${suffix}`)], app, environment);
  if (run(loom, ["test"], app, environment) !== "1 tests passed\n" ||
      run(loom, ["test", "stats"], app, environment) !== "2 tests passed\n") {
    throw new Error("relocated package/test isolation did not retain both test forms");
  }
  const expected = "2 4 23\n";
  if (run(loom, ["run", "--", "sample.txt"], app, environment) !== expected ||
      run(join(app, `wordcount${suffix}`), ["sample.txt"], app, { ...environment, LOOM_GC_STRESS: "1" }) !== expected) {
    throw new Error("relocated native file tool produced the wrong result");
  }
  // Check/editor use needs only the frontend and std, not a working LLVM bridge.
  const backend = join(prefix, `lib/loom/loom-native${suffix}`);
  renameSync(backend, `${backend}.disabled`);
  run(loom, ["check"], app, environment);
  const diagnostics = JSON.parse(run(loom, ["editor-check", app, "--tests"], app, environment));
  if (diagnostics.error || diagnostics.diagnostics.length !== 0) throw new Error("relocated editor diagnostics could not load std");
  const source = join(app, "main.loom");
  const position = readFileSync(source).indexOf(Buffer.from("summarize(text)"));
  if (position < 0) throw new Error("missing file-tool query fixture");
  const query = JSON.parse(run(loom, ["editor-query", app, "--at", source, String(position)], app, environment));
  if (query.error || !query.hover?.types?.length || !query.definitions?.length) {
    throw new Error("relocated editor queries lost checked types or definitions");
  }
  renameSync(`${backend}.disabled`, backend);
  // The new compiler can itself serve as a source-checking bootstrap seed.
  run(loom, ["check", join(root, "compiler/loom")], app, environment);
  writeSync(1, `Copying the verified toolchain to ${output}\n`);
  mkdirSync(dirname(output), { recursive: true });
  mkdirSync(output); // Never merge into or replace an existing toolchain.
  // Keep the filtered traversal above: Node's unfiltered native cpSync path
  // can terminate on Windows Unicode paths instead of throwing a JS error.
  // https://github.com/nodejs/node/issues/63970
  copyTree(prefix, output);
  run(join(output, `bin/loom${suffix}`), ["--version"], app, environment);
  console.log(`Relocated fmt/check/build/test/run, editor queries and forced-GC execution passed.\nStaged local toolchain: ${output}`);
  console.log("Invoke bin/loom by path. Native builds still require this host's LLVM libraries and linker/SDK; this is not a release archive.");
} finally {
  rmSync(temporary, { recursive: true, force: true });
}

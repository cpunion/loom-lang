#!/usr/bin/env node
// Reproduce the small, source-bound checked input used only for a Windows
// stage 0. Its producer is the normal Loom compiler's emit-checked command.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync, gzipSync } from "node:zlib";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const mode = process.argv[2];
const compiler = process.argv[3] && resolve(process.argv[3]);
if (!(["--check", "--write"].includes(mode) && compiler && process.argv.length === 4)) {
  console.error("Usage: node scripts/windows-bootstrap-seed.mjs <--check|--write> <working-loom-compiler>");
  process.exit(2);
}
if (process.platform === "win32") throw new Error("regenerate the source-bound seed on macOS or Linux");
if (!existsSync(compiler)) throw new Error(`missing Loom compiler: ${compiler}`);

const pinFile = join(root, "compiler/bootstrap/windows-stage0.source");
const archiveFile = join(root, "compiler/bootstrap/windows-stage0.checked.gz");
const sumFile = `${archiveFile}.sha256`;
const pin = readFileSync(pinFile, "utf8").trim();
if (!/^[0-9a-f]{40}$/.test(pin)) throw new Error("invalid Windows stage 0 source pin");

function run(program, args, cwd, input) {
  const result = spawnSync(program, args, { cwd, input, maxBuffer: 64 * 1024 * 1024 });
  if (result.error) throw new Error(`cannot start ${program}`, { cause: result.error });
  if (result.status !== 0) {
    throw new Error(`${program} ${args.join(" ")} failed (${result.status}):\n${result.stderr}`);
  }
  return result.stdout;
}

function sha256(data) {
  return createHash("sha256").update(data).digest("hex");
}

// Source locations appear only in emitted fault diagnostics. Emit from a
// temporary path of fixed byte length, then substitute an equally long virtual
// prefix. This preserves the checked format's byte-length fields while making
// the seed byte-identical across macOS/Linux checkout locations.
const width = 96;
const parent = realpathSync("/tmp");
const stem = "loom-stage0-";
const padding = width - Buffer.byteLength(parent) - 1 - stem.length - 6;
if (padding < 0) throw new Error("temporary root is too long for stable seed paths");
const temporary = mkdtempSync(join(parent, stem + "x".repeat(padding)));
try {
  const sourceRoot = realpathSync(temporary);
  if (Buffer.byteLength(sourceRoot) !== width) throw new Error("unexpected temporary path length");
  const identity = "/loom-bootstrap-stage0".padEnd(width, "_");
  if (Buffer.byteLength(identity) !== width) throw new Error("invalid virtual source prefix");
  let sourceArchive;
  try {
    sourceArchive = run("git", ["archive", pin, "compiler/loom", "compiler/std"], root);
  } catch (error) {
    throw new Error(`cannot recover pinned source ${pin}; fetch its Git history first`, { cause: error });
  }
  run("tar", ["-xf", "-", "-C", temporary], root, sourceArchive);
  const checked = run(compiler, ["emit-checked", join(sourceRoot, "compiler/loom"),
    "--std", join(sourceRoot, "compiler/std")], root).toString("utf8");
  if (!checked.startsWith("loom-checked-1\n") || !checked.includes(sourceRoot)) {
    throw new Error("compiler did not emit a source-bound checked program");
  }
  const normalized = Buffer.from(checked.replaceAll(sourceRoot, identity), "utf8");
  if (mode === "--write") {
    const compressed = gzipSync(normalized, { level: 9, mtime: 0 });
    writeFileSync(archiveFile, compressed);
    writeFileSync(sumFile, `${sha256(compressed)}  windows-stage0.checked.gz\n`);
    console.log(`Wrote ${archiveFile} (${compressed.length} bytes; checked SHA-256 ${sha256(normalized)})`);
  } else {
    const compressed = readFileSync(archiveFile);
    const expected = readFileSync(sumFile, "utf8").trim();
    if (expected !== `${sha256(compressed)}  windows-stage0.checked.gz`) {
      throw new Error("Windows stage 0 compressed artifact checksum mismatch");
    }
    const stored = gunzipSync(compressed);
    if (!stored.equals(normalized)) throw new Error("Windows stage 0 differs from pinned source emission");
    console.log(`Windows stage 0 matches pinned source ${pin} (${stored.length} checked bytes)`);
  }
} finally {
  rmSync(temporary, { recursive: true, force: true });
}

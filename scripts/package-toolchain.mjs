#!/usr/bin/env node
// Turn a verified local stage into a redistributable archive and test the
// archive after extraction. LLVM and the host linker remain external inputs.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { constants, copyFileSync, cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
if (args.length !== 2 || args.some(arg => arg.startsWith("-")) || !args[1].endsWith(".tar.gz")) {
  console.error("Usage: node scripts/package-toolchain.mjs <staged-directory> <new-archive.tar.gz>");
  process.exit(args.includes("--help") ? 0 : 2);
}
const stage = resolve(args[0]);
const archive = resolve(args[1]);
const checksum = `${archive}.sha256`;
if (!existsSync(stage) || !statSync(stage).isDirectory()) throw new Error(`missing staged toolchain: ${stage}`);
if (existsSync(archive) || existsSync(checksum)) throw new Error("archive or checksum destination already exists");

const suffix = process.platform === "win32" ? ".exe" : "";
const runtimeName = process.platform === "win32" ? "loom_runtime.lib" : "libloom_runtime.a";
for (const input of ["LICENSE", `bin/loom${suffix}`, `lib/loom/loom-native${suffix}`,
  `lib/loom/${runtimeName}`, "lib/loom/std"]) {
  if (!existsSync(join(stage, input))) throw new Error(`incomplete staged toolchain: missing ${input}`);
}

function run(program, arguments_, cwd, env = process.env) {
  const result = spawnSync(program, arguments_, { cwd, env, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 });
  if (result.error) throw new Error(`cannot start ${program}`, { cause: result.error });
  if (result.status !== 0) {
    throw new Error(`${program} ${arguments_.join(" ")} failed (${result.status}):\n${result.stdout}${result.stderr}`);
  }
  return result.stdout;
}

function copyTree(from, to) {
  // Keep the same filtered traversal as the staging gate: unfiltered native
  // cpSync can terminate on Windows Unicode paths (nodejs/node#63970).
  cpSync(from, to, { recursive: true,
    filter: path => !relative(from, path).split(sep).includes("target") });
}

function addRustNotices(bundle) {
  const sysroot = run("rustc", ["--print", "sysroot"], root).trim();
  const destination = join(bundle, "licenses", "rust");
  mkdirSync(destination, { recursive: true });
  for (const name of ["COPYRIGHT", "LICENSE-APACHE", "LICENSE-MIT"]) {
    const source = join(sysroot, name);
    if (existsSync(source)) copyFileSync(source, join(destination, name));
  }
  const documentation = [join(sysroot, "share/doc/rust"), join(sysroot, "share/doc/rustc")]
    .find(path => existsSync(join(path, "COPYRIGHT-library.html")));
  if (!documentation) return false;
  copyFileSync(join(documentation, "COPYRIGHT-library.html"), join(destination, "COPYRIGHT-library.html"));
  if (existsSync(join(documentation, "licenses"))) {
    copyTree(join(documentation, "licenses"), join(destination, "licenses"));
  }
  return true;
}

function addNotices(bundle) {
  const metadata = JSON.parse(run("cargo", ["metadata", "--locked", "--format-version", "1"], root));
  const packages = metadata.packages.filter(pkg => pkg.source !== null)
    .sort((a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version));
  const licenses = join(bundle, "licenses", "cargo");
  mkdirSync(licenses, { recursive: true });
  const apacheSource = metadata.packages.find(pkg => pkg.name === "inkwell" && pkg.version === "0.10.0");
  const apacheLicense = apacheSource && join(dirname(apacheSource.manifest_path), "LICENSE");
  if (!apacheLicense || !existsSync(apacheLicense)) throw new Error("missing Apache-2.0 fallback license from inkwell");
  const rows = [];
  for (const pkg of packages) {
    if (!pkg.license) throw new Error(`${pkg.name} ${pkg.version} has no declared license`);
    const sourceDir = dirname(pkg.manifest_path);
    const destination = join(licenses, `${pkg.name}-${pkg.version}`);
    mkdirSync(destination);
    const names = readdirSync(sourceDir).filter(name => /^(license|copying|notice|authors)([.-]|$)/i.test(name) &&
      statSync(join(sourceDir, name)).isFile()).sort();
    for (const name of names) copyFileSync(join(sourceDir, name), join(destination, name));
    // These two crates publish license metadata without a license file in
    // their crate tarballs. Both offer Apache-2.0; use the exact standard text
    // distributed with their dependency inkwell.
    if (!names.some(name => /^(license|copying)([.-]|$)/i.test(name)) &&
        (pkg.name === "inkwell_internals" || pkg.name === "r-efi") &&
        pkg.license.includes("Apache-2.0")) {
      copyFileSync(apacheLicense, join(destination, "LICENSE-APACHE"));
      names.push("LICENSE-APACHE");
    }
    if (!names.some(name => /^(license|copying)([.-]|$)/i.test(name))) {
      throw new Error(`${pkg.name} ${pkg.version} has no license text to distribute`);
    }
    rows.push(`| ${pkg.name} | ${pkg.version} | ${pkg.license.replaceAll("|", "\\|")} | ` +
      `${names.map(name => `licenses/cargo/${pkg.name}-${pkg.version}/${name}`).join(", ")} |`);
  }
  const detailedRustNotices = addRustNotices(bundle);
  const notice = [
    "# Third-party notices", "",
    "The native bridge and runtime use the Cargo dependencies below. This is a",
    "conservative inventory of all registry packages resolved from Cargo.lock,",
    "including build and test dependencies. Each listed license file is included",
    "under `licenses/cargo/`. The Loom source standard library and frontend are",
    "covered by the archive's top-level LICENSE. LLVM and host linker/SDK binaries",
    "are not included in this archive and retain their own licenses.", "",
    "The Rust standard library linked into the native tools is licensed MIT OR",
    "Apache-2.0, Copyright (c) The Rust Project Contributors. The packager copies",
    detailedRustNotices
      ? "its installed standard-library copyright inventory to `licenses/rust/`."
      : "available Rust license files to `licenses/rust/`; the detailed inventory is at https://github.com/rust-lang/rust/blob/1.88.0/COPYRIGHT.",
    "",
    "| Package | Version | Declared license | Included notices |",
    "| --- | --- | --- | --- |", ...rows, "",
  ];
  writeFileSync(join(bundle, "THIRD_PARTY_NOTICES.md"), notice.join("\n"));
}

const temporary = mkdtempSync(join(tmpdir(), "loom-archive-"));
try {
  const bundle = join(temporary, "loom-toolchain");
  copyTree(stage, bundle);
  copyFileSync(join(root, "distribution/INSTALL.md"), join(bundle, "INSTALL.md"));
  addNotices(bundle);
  const packagingRevision = run("git", ["rev-parse", "HEAD"], root).trim();
  const version = run(join(bundle, `bin/loom${suffix}`), ["--version"], temporary).trim();
  const buildInfo = {
    format: 1, version, packagingRevision, platform: process.platform, architecture: process.arch,
    packagerRustc: run("rustc", ["--version"], root).trim(),
    externalRequirements: ["LLVM 22 shared libraries", "host Clang/linker and SDK"],
  };
  writeFileSync(join(bundle, "BUILD_INFO.json"), `${JSON.stringify(buildInfo, null, 2)}\n`);

  const temporaryArchive = join(temporary, "loom-toolchain.tar.gz");
  // Relative tar paths work with both Windows GNU tar and macOS/BSD tar.
  run("tar", ["-czf", "loom-toolchain.tar.gz", "loom-toolchain"], temporary);

  // Verify the actual archive, rather than the stage directory used to make it.
  const unpacked = join(temporary, "unpacked 雪 with spaces");
  mkdirSync(unpacked);
  run("tar", ["-xzf", "../loom-toolchain.tar.gz"], unpacked);
  const installed = join(unpacked, "loom-toolchain");
  const application = join(temporary, "wordcount application");
  copyTree(join(root, "compiler/examples/wordcount"), application);
  const loom = join(installed, `bin/loom${suffix}`);
  const environment = { ...process.env };
  delete environment.LOOM_RUNTIME_LIBRARY;
  delete environment.LOOM_GC_STRESS;
  delete environment.LOOM_TARGET_CPU;
  run(loom, ["check"], application, environment);
  const executable = join(application, `wordcount${suffix}`);
  run(loom, ["build", "--output", executable], application, environment);
  if (run(executable, ["sample.txt"], application, { ...environment, LOOM_GC_STRESS: "1" }) !== "2 4 23\n") {
    throw new Error("extracted archive produced incorrect native output");
  }

  mkdirSync(dirname(archive), { recursive: true });
  copyFileSync(temporaryArchive, archive, constants.COPYFILE_EXCL);
  const digest = createHash("sha256").update(readFileSync(archive)).digest("hex");
  writeFileSync(checksum, `${digest}  ${basename(archive)}\n`, { flag: "wx" });
  console.log(`Verified archive: ${archive}\nSHA-256: ${digest}`);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}

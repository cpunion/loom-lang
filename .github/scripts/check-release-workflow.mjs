#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const bootstrapName = ".github/scripts/bootstrap-windows-llvm.ps1";

function requireText(errors, source, expected, label) {
  if (!source.includes(expected)) {
    errors.push(`${label}: missing ${JSON.stringify(expected)}`);
  }
}

function bootstrapInvocationErrors(source, label) {
  const errors = [];
  const marker = "./.github/scripts/bootstrap-windows-llvm.ps1";
  const index = source.indexOf(marker);
  if (index === -1) {
    return [`${label}: does not invoke ${bootstrapName}`];
  }
  const invocation = source.slice(index, index + 500);
  for (const argument of [
    "-CacheRoot",
    "-InstallRoot",
    "-EnvironmentFile",
    "-PathFile",
  ]) {
    if (!invocation.includes(argument)) {
      errors.push(`${label}: Windows LLVM bootstrap invocation omits ${argument}`);
    }
  }
  return errors;
}

export function checkReleaseWorkflow({ ci, release, bootstrap, argumentTest }) {
  const errors = [];

  for (const [label, source] of [
    ["compiler CI", ci],
    ["release workflow", release],
  ]) {
    errors.push(...bootstrapInvocationErrors(source, label));
    requireText(
      errors,
      source,
      "./.github/scripts/test-windows-llvm-bootstrap.ps1",
      label,
    );
    if (source.includes("https://github.com/llvm/llvm-project/releases/download/")) {
      errors.push(`${label}: duplicates the pinned LLVM download owned by ${bootstrapName}`);
    }
    if (source.includes("https://gitlab.gnome.org/GNOME/libxml2/")) {
      errors.push(`${label}: duplicates the pinned libxml2 download owned by ${bootstrapName}`);
    }
  }

  for (const expected of [
    "runner: windows-2025",
    "platform: windows-x86_64",
    "runtime_archive: target/release/loom_runtime.lib",
    "compiler: target/release/loomc.exe",
    "language_server: target/release/loom-lsp.exe",
    "executable_suffix: .exe",
    "Core release check/build/test/run smoke",
    "for version in core01 core02 core03",
    "C3 dual-backend release gate",
    "--test standard_values",
    "--test structured_standard_values",
    "Archive and checksum Windows release",
    "loom-lang-$env:RELEASE_NAME-$env:RELEASE_PLATFORM.zip",
    "Compress-Archive",
    "Get-FileHash -Algorithm SHA256",
    "loom-lang-$RELEASE_TAG-windows-x86_64.zip",
    "shasum -a 256 --check",
    'gh release upload "$RELEASE_TAG" "${assets[@]}"',
  ]) {
    requireText(errors, release, expected, "release workflow");
  }

  for (const expected of [
    "$llvmVersion = \"19.1.7\"",
    "$llvmArchiveSha256 = \"b4557b4f012161f56a2f5d9e877ab9635cafd7a08f7affe14829bd60c9d357f0\"",
    "$libxmlVersion = \"2.9.12\"",
    "$libxmlArchiveSha256 = \"98bfa7a9a5e2a75638422050740448ee9f02bf4dc2075c9822d7747d5ff9e617\"",
    "& curl.exe @downloadArguments",
    "& tar.exe @unpackArguments",
    "& tar.exe @libxmlUnpackArguments",
    "& cmake.exe @libxmlOptions",
    "& cmake.exe @buildArguments",
  ]) {
    requireText(errors, bootstrap, expected, "Windows LLVM bootstrap");
  }
  for (const parameter of ["CacheRoot", "InstallRoot", "EnvironmentFile", "PathFile"]) {
    requireText(errors, bootstrap, `[string]$${parameter}`, "Windows LLVM bootstrap");
  }

  for (const expected of [
    "loom bootstrap argument probe",
    "download cache",
    "LLVM install",
    "environment output.txt",
    "path output.txt",
    "-ValidateArgumentsOnly",
  ]) {
    requireText(errors, argumentTest, expected, "PowerShell argument test");
  }

  return errors;
}

export async function checkRepositoryReleaseWorkflow(root) {
  const sources = await Promise.all(
    [
      ".github/workflows/ci.yml",
      ".github/workflows/release.yml",
      bootstrapName,
      ".github/scripts/test-windows-llvm-bootstrap.ps1",
    ].map((file) => readFile(path.join(root, file), "utf8")),
  );
  return checkReleaseWorkflow({
    ci: sources[0],
    release: sources[1],
    bootstrap: sources[2],
    argumentTest: sources[3],
  });
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  const root = path.resolve(process.argv[2] ?? process.cwd());
  const errors = await checkRepositoryReleaseWorkflow(root);
  if (errors.length > 0) {
    for (const error of errors) {
      console.error(error);
    }
    process.exitCode = 1;
  } else {
    console.log("release workflow policy passed");
  }
}

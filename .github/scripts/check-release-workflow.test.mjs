import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  checkReleaseWorkflow,
  checkRepositoryReleaseWorkflow,
} from "./check-release-workflow.mjs";

const repositoryRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../..");

test("accepts the repository release and Windows bootstrap structure", async () => {
  assert.deepEqual(await checkRepositoryReleaseWorkflow(repositoryRoot), []);
});

test("requires every named PowerShell bootstrap argument", () => {
  const errors = checkReleaseWorkflow({
    ci: "./.github/scripts/bootstrap-windows-llvm.ps1 -CacheRoot x -InstallRoot y -EnvironmentFile z",
    release:
      "./.github/scripts/bootstrap-windows-llvm.ps1 -CacheRoot x -InstallRoot y -EnvironmentFile z",
    bootstrap: "",
    argumentTest: "",
  });
  assert.ok(errors.some((error) => error.includes("compiler CI") && error.includes("-PathFile")));
  assert.ok(
    errors.some((error) => error.includes("release workflow") && error.includes("-PathFile")),
  );
});

test("requires exact LF byte checks in both release archive branches", () => {
  const errors = checkReleaseWorkflow({
    ci: "",
    release: "",
    bootstrap: "",
    argumentTest: "",
  });
  for (const marker of [
    "od -An -v -tx1",
    'if [ \\"$smoke_hex\\" != \\"556e69740a\\" ]',
    "Start-Process -FilePath $smokeExecutable -RedirectStandardOutput $smokeStdout",
    "[IO.File]::ReadAllBytes($smokeStdout)",
    'if ($smokeHex -ne \\"556e69740a\\")',
  ]) {
    assert.ok(
      errors.some((error) => error.includes(marker)),
      `missing policy error for ${marker}`,
    );
  }
});

test("rejects duplicated downloads, redundant rebuilds, and unsplatted native arguments", () => {
  const errors = checkReleaseWorkflow({
    ci: [
      "./.github/scripts/bootstrap-windows-llvm.ps1 -CacheRoot x -InstallRoot y -EnvironmentFile z -PathFile p",
      "https://github.com/llvm/llvm-project/releases/download/example",
      "https://gitlab.gnome.org/GNOME/libxml2/-/archive/example",
    ].join("\n"),
    release:
      "./.github/scripts/bootstrap-windows-llvm.ps1 -CacheRoot x -InstallRoot y -EnvironmentFile z -PathFile p",
    bootstrap: [
      "& curl.exe --output archive url",
      "& tar.exe -xf archive",
      "$libxmlVersion = 'sentinel'",
      "& cmake.exe -S source -B build",
    ].join("\n"),
    argumentTest: "",
  });
  assert.ok(errors.some((error) => error.includes("duplicates the pinned LLVM download")));
  assert.ok(errors.some((error) => error.includes("retired static libxml2 rebuild")));
  assert.ok(errors.some((error) => error.includes("@downloadArguments")));
  assert.ok(errors.some((error) => error.includes("@unpackArguments")));
  assert.ok(errors.some((error) => error.includes("official LLVM-C DLL/import library")));
});

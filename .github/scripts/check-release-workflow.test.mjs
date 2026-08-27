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
    ci: "bootstrap-windows-llvm.ps1 -CacheRoot x -InstallRoot y -EnvironmentFile z",
    release: "bootstrap-windows-llvm.ps1 -CacheRoot x -InstallRoot y -EnvironmentFile z",
    bootstrap: "",
    argumentTest: "",
  });
  assert.ok(errors.some((error) => error.includes("compiler CI") && error.includes("-PathFile")));
  assert.ok(
    errors.some((error) => error.includes("release workflow") && error.includes("-PathFile")),
  );
});

test("rejects duplicated download policy and unsplatted native arguments", () => {
  const errors = checkReleaseWorkflow({
    ci: [
      "bootstrap-windows-llvm.ps1 -CacheRoot x -InstallRoot y -EnvironmentFile z -PathFile p",
      "https://github.com/llvm/llvm-project/releases/download/example",
    ].join("\n"),
    release:
      "bootstrap-windows-llvm.ps1 -CacheRoot x -InstallRoot y -EnvironmentFile z -PathFile p",
    bootstrap: "& curl.exe --output archive url\n& cmake.exe -S source -B build",
    argumentTest: "",
  });
  assert.ok(errors.some((error) => error.includes("duplicates the pinned LLVM download")));
  assert.ok(errors.some((error) => error.includes("@downloadArguments")));
  assert.ok(errors.some((error) => error.includes("@libxmlOptions")));
});

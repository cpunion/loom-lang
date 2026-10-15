import assert from "node:assert/strict";
import test from "node:test";
import { releasePlan, checkArchiveInfo, checkUnusedTag } from "./draft-toolchain-release.mjs";

const repository = { full_name: "cpunion/loom-lang" };
const run = {
  repository, head_repository: repository, path: ".github/workflows/ci.yml",
  head_branch: "main", event: "push", status: "completed", conclusion: "success",
  head_sha: "a".repeat(40), html_url: "https://github.com/cpunion/loom-lang/actions/runs/123",
};
const jobs = ["macOS", "Linux", "Windows"].map(host => ({
  name: `${host} native compiler (LLVM 22)`, status: "completed", conclusion: "success",
}));
const artifacts = ["macOS-ARM64", "Linux-X64", "Windows-X64"].map(host => ({
  name: `loom-toolchain-${host}`, expired: false,
}));

test("release planning pins successful main evidence and all three host archives", () => {
  const plan = releasePlan(run, jobs, artifacts, "v0.1.0-dev.1");
  assert.equal(plan.commit, run.head_sha);
  assert.deepEqual(plan.artifacts.map(item => [item.platform, item.architecture]),
    [["darwin", "arm64"], ["linux", "x64"], ["win32", "x64"]]);
  const info = { format: 1, packagingRevision: plan.commit, platform: "darwin", architecture: "arm64", version: "loom development" };
  checkArchiveInfo(info, plan, plan.artifacts[0]);
  for (const changed of [{ packagingRevision: "b".repeat(40) }, { architecture: "x64" }, { platform: "linux" }]) {
    assert.throws(() => checkArchiveInfo({ ...info, ...changed }, plan, plan.artifacts[0]));
  }
});

test("failed, fork, PR, wrong workflow, missing and expired evidence cannot release", () => {
  for (const changed of [
    { conclusion: "failure" }, { status: "in_progress" }, { head_branch: "feature" },
    { event: "pull_request" }, { path: ".github/workflows/other.yml" },
    { head_repository: { full_name: "another/fork" } },
  ]) {
    assert.throws(() => releasePlan({ ...run, ...changed }, jobs, artifacts, "v0.1.0-dev.1"));
  }
  assert.throws(() => releasePlan(run, jobs.slice(1), artifacts, "v0.1.0-dev.1"));
  assert.throws(() => releasePlan(run, jobs, artifacts.slice(1), "v0.1.0-dev.1"));
  assert.throws(() => releasePlan(run, jobs, artifacts.map(item => ({ ...item, expired: true })), "v0.1.0-dev.1"));
});

test("development release tags are explicit SemVer prereleases, not arbitrary refs", () => {
  checkUnusedTag([], "v0.1.0-dev.1");
  checkUnusedTag([{ ref: "refs/tags/v0.1.0-dev.10" }], "v0.1.0-dev.1");
  assert.throws(() => checkUnusedTag([{ ref: "refs/tags/v0.1.0-dev.1" }], "v0.1.0-dev.1"));
  for (const tag of ["v0.1.0-dev.1", "v1.2.3-rc.0+build.01"]) {
    assert.equal(releasePlan(run, jobs, artifacts, tag).tag, tag);
  }
  for (const tag of ["v0.1.0", "main", "--latest", "v01.2.3-dev", "v1.2.3-rc.01", "v1.2.3-", "v1.2.3-dev/1", "v1.2.3-dev\n"]) {
    assert.throws(() => releasePlan(run, jobs, artifacts, tag));
  }
});

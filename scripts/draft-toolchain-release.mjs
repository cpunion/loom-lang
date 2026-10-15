#!/usr/bin/env node
// Promote existing, successful main CI archives. Never build again, execute an
// archive, guess a version, overwrite a release, or publish beyond a draft.
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream, mkdtempSync, readFileSync, readdirSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";

const repository = "cpunion/loom-lang";
const hosts = [
  ["macOS", "darwin", "macOS native compiler (LLVM 22)"],
  ["Linux", "linux", "Linux native compiler (LLVM 22)"],
  ["Windows", "win32", "Windows native compiler (LLVM 22)"],
];

export function releasePlan(run, jobs, artifacts, tag) {
  const version = /^v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/.exec(tag);
  if (!version || version[0] !== tag || version[4].split(".").some(part => /^0\d+$/.test(part))) {
    throw new Error("choose an explicit SemVer prerelease tag, such as v0.1.0-dev.1");
  }
  if (run.repository?.full_name !== repository || run.head_repository?.full_name !== repository ||
      run.path !== ".github/workflows/ci.yml" || run.head_branch !== "main" ||
      !["push", "workflow_dispatch"].includes(run.event) ||
      run.status !== "completed" || run.conclusion !== "success" ||
      !/^[0-9a-f]{40}$/.test(run.head_sha)) {
    throw new Error("release input must be a successful compiler-ci run from this repository main");
  }
  const selected = hosts.map(([host, platform, job]) => {
    if (!jobs.some(item => item.name === job && item.status === "completed" && item.conclusion === "success")) {
      throw new Error(`missing successful platform gate: ${host}`);
    }
    const available = artifacts.filter(item => !item.expired &&
      new RegExp(`^loom-toolchain-${host}-(ARM64|X64)$`).test(item.name));
    if (available.length !== 1) throw new Error(`expected one unexpired ${host} toolchain artifact`);
    const name = available[0].name;
    return { name, platform, architecture: name.endsWith("-ARM64") ? "arm64" : "x64" };
  });
  return { repository, tag, commit: run.head_sha, run: run.html_url, artifacts: selected };
}

export function checkArchiveInfo(info, plan, artifact) {
  if (info.format !== 1 || info.packagingRevision !== plan.commit ||
      info.platform !== artifact.platform || info.architecture !== artifact.architecture ||
      typeof info.version !== "string" || info.version.length === 0) {
    throw new Error(`archive build identity does not match its CI run: ${artifact.name}`);
  }
}

function command(program, args) {
  const result = spawnSync(program, args, { encoding: "utf8", maxBuffer: 8 * 1024 * 1024 });
  if (result.error || result.status !== 0) {
    throw new Error(`${program} failed: ${result.stderr || result.error?.message || result.status}`);
  }
  return result.stdout;
}

function api(path) {
  return JSON.parse(command("gh", ["api", `repos/${repository}/${path}`]));
}

async function digest(file) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest("hex");
}

async function main(args) {
  if (args.includes("--help")) {
    console.log("Usage: node scripts/draft-toolchain-release.mjs <successful-main-ci-run> <vSEMVER-prerelease> [--dry-run]");
    return;
  }
  if ((args.length !== 2 && args.length !== 3) || !/^\d+$/.test(args[0]) ||
      (args.length === 3 && args[2] !== "--dry-run")) {
    throw new Error("provide a numeric CI run, explicit prerelease tag, and optional --dry-run");
  }
  const [id, tag] = args;
  const run = api(`actions/runs/${id}`);
  const jobs = api(`actions/runs/${id}/jobs?per_page=100`).jobs;
  const artifacts = api(`actions/runs/${id}/artifacts?per_page=100`).artifacts;
  const plan = releasePlan(run, jobs, artifacts, tag);
  const directory = mkdtempSync(join(tmpdir(), "loom-release-"));
  try {
    const assets = [];
    const checksums = [];
    for (const artifact of plan.artifacts) {
      const destination = join(directory, artifact.name);
      command("gh", ["run", "download", id, "--repo", repository,
        "--name", artifact.name, "--dir", destination]);
      const archive = `${artifact.name}.tar.gz`;
      const checksum = `${archive}.sha256`;
      const files = readdirSync(destination).sort();
      if (JSON.stringify(files) !== JSON.stringify([archive, checksum])) {
        throw new Error(`unexpected files in ${artifact.name}`);
      }
      const file = join(destination, archive);
      const expected = /^([0-9a-f]{64})  ([^\r\n]+)\n$/.exec(readFileSync(join(destination, checksum), "utf8"));
      const actual = await digest(file);
      if (!expected || expected[2] !== basename(file) || expected[1] !== actual) {
        throw new Error(`archive checksum does not match downloaded bytes: ${archive}`);
      }
      const info = JSON.parse(command("tar", ["-xOf", file, "loom-toolchain/BUILD_INFO.json"]));
      checkArchiveInfo(info, plan, artifact);
      assets.push(file, join(destination, checksum));
      checksums.push({ archive, sha256: actual });
    }
    console.log(JSON.stringify({ ...plan, checksums }, null, 2));
    if (args.includes("--dry-run")) return;
    const notes = [
      `Development toolchains from compiler-ci: ${plan.run}`, "",
      `Source commit: ${plan.commit}`, "",
      "All three host bootstrap/native/archive gates passed for that commit.",
      "These are experimental development builds, not completion of the language design.",
      "LLVM 22 shared libraries and the host Clang/linker/SDK remain external requirements.",
      "See the included INSTALL.md and third-party notices. Check each SHA-256 before extraction.",
    ].join("\n");
    command("gh", ["release", "create", tag, ...assets, "--repo", repository,
      "--target", plan.commit, "--draft", "--prerelease", "--title", `Loom ${tag}`, "--notes", notes]);
    console.log(`Created draft ${tag}; review and publish it explicitly.`);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  main(process.argv.slice(2)).catch(error => {
    console.error(error.message);
    process.exitCode = 1;
  });
}

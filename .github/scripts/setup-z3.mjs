// One pinned solver on every CI platform; no Python or native build dependency.
import { createHash } from "node:crypto";
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

const releases = {
  "darwin-arm64": ["arm64-osx-13.3", "81d29e934fd863079a74af35eecaeaef8047e0e12414d33ca322b358d68383db"],
  "darwin-x64": ["x64-osx-13.3", "f25cabdb01a32942a42227c36902c0846c759dcaabef4aca3b2e86331447f6ff"],
  "linux-x64": ["x64-glibc-2.39", "f47be8d27d3230e823bf1eeede2fe0abaca55bb78d0b59974370e6689a92284a"],
  "win32-x64": ["x64-win", "6ff368eaa2f9dc18acf244e736a074df877026bfc83abede6f327ebb95680535"],
};
const release = releases[`${process.platform}-${process.arch}`];
if (!release || !process.env.RUNNER_TEMP || !process.env.GITHUB_PATH) {
  throw new Error("setup-z3 requires a supported GitHub Actions runner");
}
const [platform, checksum] = release;
const name = `z3-5.1.0-${platform}`;
const directory = join(process.env.RUNNER_TEMP, "loom-z3");
mkdirSync(directory, { recursive: true });
const archive = join(directory, `${name}.zip`);
const response = await fetch(`https://github.com/Z3Prover/z3/releases/download/z3-5.1.0/${name}.zip`);
if (!response.ok) throw new Error(`Z3 download failed: ${response.status}`);
const bytes = Buffer.from(await response.arrayBuffer());
if (createHash("sha256").update(bytes).digest("hex") !== checksum) {
  throw new Error("Z3 archive checksum mismatch");
}
writeFileSync(archive, bytes);
function run(executable, args) {
  const output = spawnSync(executable, args, { stdio: "inherit" });
  if (output.error || output.status !== 0) throw new Error(`${executable} failed`, { cause: output.error });
}
if (process.platform === "win32") {
  // Environment variables avoid interpolating runner paths into PowerShell.
  process.env.LOOM_Z3_ARCHIVE = archive;
  process.env.LOOM_Z3_DIRECTORY = directory;
  run("pwsh", ["-NoProfile", "-Command",
    "Expand-Archive -LiteralPath $env:LOOM_Z3_ARCHIVE -DestinationPath $env:LOOM_Z3_DIRECTORY"]);
} else {
  run("unzip", ["-q", archive, "-d", directory]);
}
const bin = join(directory, name, "bin");
run(join(bin, process.platform === "win32" ? "z3.exe" : "z3"), ["--version"]);
appendFileSync(process.env.GITHUB_PATH, `${bin}\n`);

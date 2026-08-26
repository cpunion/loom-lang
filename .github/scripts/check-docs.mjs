#!/usr/bin/env node

import { lstat, readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const ignoredDirectories = new Set([".git", "target"]);
const hanPattern = /\p{Script=Han}/u;
const inlineLinkPattern = /!?\[[^\]]*\]\((?:<([^>]+)>|([^\s)]+))(?:\s+[^)]*)?\)/g;
const referenceLinkPattern = /^\s*\[[^\]]+\]:\s*(?:<([^>]+)>|(\S+))/gm;
const externalTargetPattern = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i;

async function collectFiles(root, relative = "") {
  const directory = path.join(root, relative);
  const entries = await readdir(directory, { withFileTypes: true });
  const files = [];

  for (const entry of entries.sort((left, right) => left.name.localeCompare(right.name))) {
    if (entry.isDirectory() && ignoredDirectories.has(entry.name)) {
      continue;
    }

    const child = path.posix.join(relative.split(path.sep).join(path.posix.sep), entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectFiles(root, child)));
    } else if (entry.isFile()) {
      files.push(child);
    }
  }

  return files;
}

function lineAndColumn(source, offset) {
  const prefix = source.slice(0, offset);
  const lines = prefix.split("\n");
  return { line: lines.length, column: [...lines.at(-1)].length + 1 };
}

function proseForLinks(source) {
  let fence = null;
  return source
    .split("\n")
    .map((line) => {
      const marker = line.match(/^ {0,3}(`{3,}|~{3,})/);
      if (fence !== null) {
        if (
          marker &&
          marker[1][0] === fence.character &&
          marker[1].length >= fence.length &&
          line.slice(marker[0].length).trim() === ""
        ) {
          fence = null;
        }
        return "";
      }
      if (marker) {
        fence = { character: marker[1][0], length: marker[1].length };
        return "";
      }
      return line.replace(/(`+).*?\1/g, "");
    })
    .join("\n");
}

function linkTargets(source) {
  const prose = proseForLinks(source);
  const targets = [];
  for (const pattern of [inlineLinkPattern, referenceLinkPattern]) {
    pattern.lastIndex = 0;
    for (const match of prose.matchAll(pattern)) {
      targets.push(match[1] ?? match[2]);
    }
  }
  return targets;
}

function cleanTarget(target) {
  const trimmed = target.trim();
  if (!trimmed || trimmed.startsWith("#") || externalTargetPattern.test(trimmed)) {
    return null;
  }

  const withoutFragment = trimmed.split("#", 1)[0].split("?", 1)[0];
  try {
    return decodeURIComponent(withoutFragment);
  } catch {
    return withoutFragment;
  }
}

async function resolveLocalTarget(root, sourceFile, target) {
  const decoded = cleanTarget(target);
  if (decoded === null) {
    return { kind: "ignored" };
  }

  const absoluteRoot = path.resolve(root);
  const candidate = path.resolve(root, path.dirname(sourceFile), decoded);
  if (candidate !== absoluteRoot && !candidate.startsWith(`${absoluteRoot}${path.sep}`)) {
    return { kind: "outside", target: decoded };
  }

  try {
    const metadata = await lstat(candidate);
    if (metadata.isSymbolicLink()) {
      return { kind: "symlink", target: decoded };
    }
    if (metadata.isDirectory()) {
      const index = path.join(candidate, "README.md");
      const indexMetadata = await lstat(index);
      if (!indexMetadata.isFile() || indexMetadata.isSymbolicLink()) {
        return { kind: "missing", target: decoded };
      }
      return {
        kind: "file",
        file: path.relative(root, index).split(path.sep).join(path.posix.sep),
      };
    }
    if (!metadata.isFile()) {
      return { kind: "missing", target: decoded };
    }
    return {
      kind: "file",
      file: path.relative(root, candidate).split(path.sep).join(path.posix.sep),
    };
  } catch {
    return { kind: "missing", target: decoded };
  }
}

export async function checkDocumentation(root) {
  const allFiles = await collectFiles(root);
  const markdownFiles = allFiles.filter((file) => file.endsWith(".md"));
  const markdownSet = new Set(markdownFiles);
  const graph = new Map(markdownFiles.map((file) => [file, new Set()]));
  const errors = [];

  for (const file of markdownFiles) {
    const source = await readFile(path.join(root, file), "utf8");
    const firstContent = source.split("\n").find((line) => line.trim() !== "");
    if (!firstContent?.startsWith("# ")) {
      errors.push(`${file}:1: every Markdown document must begin with one H1 heading`);
    }

    const han = source.match(hanPattern);
    if (han?.index !== undefined) {
      const location = lineAndColumn(source, han.index);
      errors.push(
        `${file}:${location.line}:${location.column}: documentation must be written in English`,
      );
    }

    for (const target of linkTargets(source)) {
      const resolved = await resolveLocalTarget(root, file, target);
      if (resolved.kind === "ignored") {
        continue;
      }
      if (resolved.kind !== "file") {
        errors.push(`${file}: invalid local link ${JSON.stringify(target)} (${resolved.kind})`);
        continue;
      }
      if (resolved.file.endsWith(".md") && markdownSet.has(resolved.file)) {
        graph.get(file).add(resolved.file);
      }
    }
  }

  const reachable = new Set();
  const pending = markdownSet.has("README.md") ? ["README.md"] : [];
  while (pending.length > 0) {
    const file = pending.pop();
    if (reachable.has(file)) {
      continue;
    }
    reachable.add(file);
    for (const destination of graph.get(file) ?? []) {
      pending.push(destination);
    }
  }

  for (const file of markdownFiles) {
    if (file.startsWith("docs/") && !reachable.has(file)) {
      errors.push(`${file}: document is not reachable from README.md`);
    }
  }

  return errors.sort();
}

async function main() {
  const root = path.resolve(process.argv[2] ?? process.cwd());
  const errors = await checkDocumentation(root);
  if (errors.length > 0) {
    for (const error of errors) {
      console.error(error);
    }
    process.exitCode = 1;
    return;
  }

  console.log("Documentation structure, language, and local links are valid.");
}

if (path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await main();
}

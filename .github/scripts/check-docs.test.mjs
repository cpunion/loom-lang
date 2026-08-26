import assert from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { checkDocumentation } from "./check-docs.mjs";

async function fixture(files) {
  const root = await mkdtemp(path.join(os.tmpdir(), "loom-docs-"));
  for (const [name, source] of Object.entries(files)) {
    const destination = path.join(root, name);
    await mkdir(path.dirname(destination), { recursive: true });
    await writeFile(destination, source, "utf8");
  }
  return root;
}

test("accepts an English, connected documentation graph", async (context) => {
  const root = await fixture({
    "README.md": "# Project\n\nRead the [documentation](docs/README.md).\n",
    "docs/README.md": "# Documentation\n\nRead the [guide](guide.md).\n",
    "docs/guide.md": "# Guide\n\nInspect the [manifest](../loom.toml).\n",
    "loom.toml": "[package]\nname = \"sample\"\n",
  });
  context.after(() => rm(root, { recursive: true, force: true }));

  assert.deepEqual(await checkDocumentation(root), []);
});

test("rejects non-English text, broken links, missing H1s, and orphaned pages", async (context) => {
  const root = await fixture({
    "README.md": "# Project\n\n[Docs](docs/README.md) and [missing](missing.md).\n",
    "docs/README.md": "Documentation only.\n\n中文\n",
    "docs/orphan.md": "# Orphan\n",
  });
  context.after(() => rm(root, { recursive: true, force: true }));

  const errors = await checkDocumentation(root);
  assert.ok(errors.some((error) => error.includes("documentation must be written in English")));
  assert.ok(errors.some((error) => error.includes("must begin with one H1")));
  assert.ok(errors.some((error) => error.includes('invalid local link "missing.md"')));
  assert.ok(errors.some((error) => error.includes("docs/orphan.md: document is not reachable")));
});

test("ignores generated and version-control directories", async (context) => {
  const root = await fixture({
    "README.md": "# Project\n",
    "target/generated.md": "中文\n",
    ".git/internal.md": "中文\n",
  });
  context.after(() => rm(root, { recursive: true, force: true }));

  assert.deepEqual(await checkDocumentation(root), []);
});

test("does not interpret code as Markdown links", async (context) => {
  const root = await fixture({
    "README.md": [
      "# Project",
      "",
      "`fn value[T](input)` is an inline signature.",
      "",
      "```loom",
      "fn smaller[T: Ordered](left T, right T) T",
      "```",
      "",
    ].join("\n"),
  });
  context.after(() => rm(root, { recursive: true, force: true }));

  assert.deepEqual(await checkDocumentation(root), []);
});

#!/usr/bin/env node
// Fault-injection fixture: queries and recovery use real SQLite, while the
// upgrade process fails before any of its generated SQL executes.
import { spawnSync } from "node:child_process";

const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const input = Buffer.concat(chunks);
if (input.includes(Buffer.from("CREATE TRIGGER loom_positive_insert"))) {
  process.exit(1);
}
const result = spawnSync("/usr/bin/sqlite3", process.argv.slice(2), { input });
if (result.stdout) process.stdout.write(result.stdout);
if (result.stderr) process.stderr.write(result.stderr);
process.exit(result.status ?? 1);

#!/usr/bin/env node
// Execute the real transaction, then lose its successful acknowledgement.
// Queries and standalone ledger appends retain their actual results.
import { spawnSync } from "node:child_process";

const chunks = [];
for await (const chunk of process.stdin) chunks.push(chunk);
const input = Buffer.concat(chunks);
const result = spawnSync("/usr/bin/sqlite3", process.argv.slice(2), { input });
const committed = result.status === 0 && input.includes(Buffer.from("BEGIN IMMEDIATE;")) &&
  input.includes(Buffer.from("COMMIT;"));
if (!committed && result.stdout) process.stdout.write(result.stdout);
if (result.stderr) process.stderr.write(result.stderr);
process.exitCode = committed ? 1 : result.status ?? 1;

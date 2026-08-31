#!/usr/bin/env node

import { lstat, readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const maxArchiveBytes = 256 * 1024 * 1024;

export const removedRuntimeSymbols = Object.freeze([
  "loom_gc_root_push_v1",
  "loom_gc_root_pop_v1",
  "loom_gc_alloc_value",
  "loom_gc_alloc_value_node",
  "loom_gc_clone_value_v1",
  "loom_gc_build_value_nodes_v1",
  "loom_gc_clone_witness_v1",
  "loom_runtime_list_add",
  "loom_runtime_list_get",
  "loom_runtime_text_get",
  "loom_runtime_text_concat",
  "loom_runtime_bytes_append",
  "loom_runtime_bytes_get",
  "loom_runtime_bytes_decode_utf8",
  "loom_runtime_path_contains_nul",
  "loom_runtime_path_join",
  "loom_runtime_text_map_get",
  "loom_runtime_text_map_insert",
  "loom_runtime_text_map_remove",
  "loom_runtime_json_format",
  "loom_runtime_format_float",
  "loom_runtime_log",
  "loom_runtime_set_arguments",
  "loom_runtime_process_arguments",
  "loom_runtime_process_environment",
  "loom_file_open_read",
  "loom_file_create",
  "loom_file_try_open_read",
  "loom_file_try_create",
  "loom_file_read_text",
  "loom_file_try_read_text",
  "loom_file_write_text",
  "loom_file_try_write_text",
  "loom_socket_connect",
  "loom_socket_try_connect",
  "loom_socket_read_text",
  "loom_socket_try_read_text",
  "loom_socket_write_text",
  "loom_socket_try_write_text",
  "loom_io_close",
  "loom_int_list_reserve_v1",
  "loom_int_list_drop_v1",
  "loom_task_trace_live_slots",
  "loom_task_spawn",
  "loom_task_spawn_descriptor",
  "loom_task_capture_witnesses_v1",
  "loom_task_witness_v1",
  "loom_task_from_wait_source",
  "loom_task_slot",
  "loom_task_result",
  "loom_task_state",
  "loom_task_set_state",
  "loom_task_is_cancelled",
  "loom_task_set_fault",
  "loom_task_join_result",
  "loom_task_write_join_result",
  "loom_task_suspend_value",
  "loom_task_suspend_wait",
  "loom_task_cancel",
  "loom_join_create",
  "loom_join_task",
  "loom_join_add_task",
  "loom_join_add_list",
]);

function isSymbolByte(byte) {
  return (
    (byte >= 48 && byte <= 57) ||
    (byte >= 65 && byte <= 90) ||
    byte === 95 ||
    (byte >= 97 && byte <= 122)
  );
}

function containsExactSymbol(archive, symbol) {
  const needle = Buffer.from(symbol, "ascii");
  let offset = archive.indexOf(needle);
  while (offset !== -1) {
    const previous = offset - 1;
    const hasBoundaryBefore =
      offset === 0 ||
      !isSymbolByte(archive[previous]) ||
      (archive[previous] === 95 &&
        (offset === 1 || !isSymbolByte(archive[previous - 1])));
    const next = offset + needle.length;
    if (
      hasBoundaryBefore &&
      (next === archive.length || !isSymbolByte(archive[next]))
    ) {
      return true;
    }
    offset = archive.indexOf(needle, offset + 1);
  }
  return false;
}

export function findRemovedRuntimeSymbols(archive) {
  return removedRuntimeSymbols.filter((symbol) =>
    containsExactSymbol(archive, symbol),
  );
}

export async function checkRuntimeArchive(archivePath) {
  const metadata = await lstat(archivePath);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    throw new Error(`runtime archive is not a regular file: ${archivePath}`);
  }
  if (metadata.size > maxArchiveBytes) {
    throw new Error(
      `runtime archive exceeds ${maxArchiveBytes} bytes: ${archivePath}`,
    );
  }
  const archive = await readFile(archivePath);
  if (
    archive.length < 8 ||
    archive.subarray(0, 8).toString("ascii") !== "!<arch>\n"
  ) {
    throw new Error(`runtime archive has an invalid ar signature: ${archivePath}`);
  }
  return findRemovedRuntimeSymbols(archive);
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  const archivePath = process.argv[2];
  if (!archivePath) {
    console.error("usage: check-runtime-archive-symbols.mjs RUNTIME_ARCHIVE");
    process.exitCode = 2;
  } else {
    try {
      const matches = await checkRuntimeArchive(archivePath);
      if (matches.length > 0) {
        for (const match of matches) {
          console.error(`runtime archive contains removed ABI symbol: ${match}`);
        }
        process.exitCode = 1;
      } else {
        console.log("runtime archive contains no removed universal ABI symbols");
      }
    } catch (error) {
      console.error(error instanceof Error ? error.message : String(error));
      process.exitCode = 2;
    }
  }
}

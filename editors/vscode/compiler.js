'use strict';
const { execFile } = require('node:child_process');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');

function run(executable, args, cwd, signal, input = '') {
  return new Promise((resolve, reject) => {
    const child = execFile(executable, args, { cwd, signal, windowsHide: true, maxBuffer: 16 * 1024 * 1024 }, (error, stdout, stderr) => {
      if (error && typeof error.code !== 'number') reject(error);
      else resolve({ code: error?.code ?? 0, stdout, stderr });
    });
    child.stdin.on('error', () => {}); // A rejecting compiler may close stdin early.
    child.stdin.end(input);
  });
}

async function snapshots(documents) {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-editor-'));
  try {
    const args = [];
    for (let index = 0; index < documents.length; index++) {
      const snapshot = path.join(directory, String(index));
      await fs.writeFile(snapshot, documents[index].text, 'utf8');
      args.push('--overlay', documents[index].path, snapshot);
    }
    return { args, dispose: () => fs.rm(directory, { recursive: true, force: true }) };
  } catch (error) {
    await fs.rm(directory, { recursive: true, force: true });
    throw error;
  }
}

async function editorReport(settings, directory, mode, extraArgs, overlayArgs, signal) {
  const args = [mode, directory, '--tests', ...extraArgs, ...overlayArgs];
  if (settings.stdRoot) args.push('--std', settings.stdRoot);
  const result = await run(settings.executable, args, directory, signal);
  if (result.code !== 0 && result.code !== 1) throw new Error(result.stderr || `Compiler exited ${result.code}`);
  let report;
  try { report = JSON.parse(result.stdout); }
  catch { throw new Error(result.stderr || `Compiler did not return ${mode} JSON; check the configured Loom executable.`); }
  if (!Array.isArray(report.diagnostics)) throw new Error(`Invalid ${mode} diagnostics response.`);
  return report;
}

function check(settings, directory, overlayArgs, signal) {
  return editorReport(settings, directory, 'editor-check', [], overlayArgs, signal);
}

function query(settings, directory, file, offset, overlayArgs, signal, complete = false) {
  return editorReport(settings, directory, complete ? 'editor-complete' : 'editor-query', ['--at', file, String(offset)], overlayArgs, signal);
}

async function format(settings, directory, text, signal) {
  const result = await run(settings.executable, ['fmt', '--stdin'], directory, signal, text);
  if (result.code !== 0) throw new Error(result.stderr || 'Loom could not format this buffer.');
  return result.stdout;
}

function bytePosition(document, offset) {
  const bytes = Buffer.from(document.getText(), 'utf8');
  return document.positionAt(bytes.subarray(0, Math.max(0, offset)).toString('utf8').length);
}

function byteOffset(document, position) {
  return Buffer.byteLength(document.getText().slice(0, document.offsetAt(position)), 'utf8');
}

module.exports = { snapshots, check, query, format, bytePosition, byteOffset };

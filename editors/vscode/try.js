'use strict';
const { spawn } = require('node:child_process');
const { existsSync } = require('node:fs');
const path = require('node:path');

const extension = path.resolve(__dirname);
const compiler = path.resolve(extension, '../../target', process.platform === 'win32' ? 'loom.exe' : 'loom');
const executable = process.env.VSCODE_EXECUTABLE || (process.platform === 'win32' ? 'Code.exe' : 'code');

if (!existsSync(compiler)) {
  console.error(`Loom compiler not found: ${compiler}\nRun bash scripts/bootstrap.sh from the repository root, then retry.`);
  process.exitCode = 1;
} else {
  const child = spawn(executable, [
    '--new-window',
    `--extensionDevelopmentPath=${extension}`,
    path.join(extension, 'wordcount.code-workspace'),
  ], { stdio: 'inherit' });
  child.on('error', error => {
    console.error(`Could not launch VS Code (${executable}): ${error.message}\nInstall VS Code or set VSCODE_EXECUTABLE to its launcher.`);
    process.exitCode = 1;
  });
  child.on('exit', code => { process.exitCode = code ?? 1; });
}

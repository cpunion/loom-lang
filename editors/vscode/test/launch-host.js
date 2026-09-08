'use strict';
const { spawn } = require('node:child_process');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');

async function main() {
  const extension = path.resolve(__dirname, '..');
  const repository = path.resolve(extension, '../..');
  const profile = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-vscode-test-'));
  try {
    const project = path.join(profile, 'project');
    await fs.mkdir(project);
    await fs.copyFile(path.join(__dirname, 'fixtures/project/main.loom'), path.join(project, 'main.loom'));
    const child = spawn(process.env.VSCODE_EXECUTABLE || (process.platform === 'win32' ? 'Code.exe' : 'code'), [
      '--new-window', '--wait', '--skip-welcome', '--skip-release-notes', '--disable-workspace-trust', '--disable-extensions',
      `--user-data-dir=${path.join(profile, 'data')}`, `--extensions-dir=${path.join(profile, 'extensions')}`,
      `--extensionDevelopmentPath=${extension}`, `--extensionTestsPath=${path.join(__dirname, 'host.js')}`,
      project,
    ], { stdio: 'inherit', env: { ...process.env,
      LOOM_EDITOR_COMPILER: process.env.LOOM_EDITOR_COMPILER || path.join(repository, 'target', process.platform === 'win32' ? 'loom.exe' : 'loom'),
      LOOM_EDITOR_STD: process.env.LOOM_EDITOR_STD || path.join(repository, 'compiler/std'),
      LOOM_EDITOR_REPOSITORY: repository,
    } });
    const code = await new Promise((resolve, reject) => { child.on('error', reject); child.on('exit', resolve); });
    if (code !== 0) throw new Error(`VS Code extension-host smoke exited ${code}`);
    console.log('VS Code activation, unsaved diagnostics, hover, definition, formatting/save, test, and run passed.');
  } finally { await fs.rm(profile, { recursive: true, force: true }); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });

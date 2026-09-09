'use strict';
const { spawn } = require('node:child_process');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');

async function main() {
  const extension = path.resolve(__dirname, '..');
  const repository = path.resolve(extension, '../..');
  const profile = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-vscode-test-'));
  const logFile = path.join(profile, 'launcher.log');
  let log;
  try {
    const project = path.join(profile, 'project');
    const resultFile = path.join(profile, 'result.json');
    await fs.mkdir(project);
    await fs.copyFile(path.join(__dirname, 'fixtures/project/main.loom'), path.join(project, 'main.loom'));
    log = await fs.open(logFile, 'w');
    // --verbose waits for the app process (also through macOS `open`); --wait
    // instead waits for an editor file marker that test-runner exit can bypass.
    const child = spawn(process.env.VSCODE_EXECUTABLE || (process.platform === 'win32' ? 'Code.exe' : 'code'), [
      '--new-window', '--verbose', '--skip-welcome', '--skip-release-notes', '--disable-workspace-trust', '--disable-extensions',
      `--user-data-dir=${path.join(profile, 'data')}`, `--extensions-dir=${path.join(profile, 'extensions')}`,
      `--extensionDevelopmentPath=${extension}`, `--extensionTestsPath=${path.join(__dirname, 'host.js')}`,
      project,
    ], { stdio: ['ignore', log.fd, log.fd], env: { ...process.env,
      LOOM_EDITOR_COMPILER: process.env.LOOM_EDITOR_COMPILER || path.join(repository, 'target', process.platform === 'win32' ? 'loom.exe' : 'loom'),
      LOOM_EDITOR_STD: process.env.LOOM_EDITOR_STD || path.join(repository, 'compiler/std'),
      LOOM_EDITOR_REPOSITORY: repository,
      LOOM_EDITOR_HOST_RESULT: resultFile,
    } });
    const code = await new Promise((resolve, reject) => { child.on('error', reject); child.on('exit', resolve); });
    const result = await fs.readFile(resultFile, 'utf8').then(JSON.parse).catch(() => null);
    if (!result) throw new Error(`VS Code exited ${code} without a host-test completion report`);
    if (code !== 0 || result.passed !== true) throw new Error(result.error || `VS Code extension-host smoke exited ${code}`);
    console.log('VS Code activation, unsaved diagnostics, hover, definition, name completion, formatting/save, test, and run passed.');
  } catch (error) {
    const output = await fs.readFile(logFile, 'utf8').catch(() => '');
    if (output) process.stderr.write(output.slice(-8192));
    throw error;
  } finally {
    await log?.close();
    await fs.rm(profile, { recursive: true, force: true });
  }
}
main().catch(error => { console.error(error); process.exitCode = 1; });

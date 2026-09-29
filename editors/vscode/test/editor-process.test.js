'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const { readFileSync } = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const childProcess = require('node:child_process');
const { EventEmitter } = require('node:events');
const { PassThrough, Writable } = require('node:stream');

test('resident workers refresh replaced compilers and retain serialized cancellation', async t => {
  const directory = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-worker-'));
  const name = process.platform === 'win32' ? 'loom.exe' : 'loom';
  const executable = path.join(directory, name);
  const previousPath = process.env.PATH;
  let starts = 0, stops = 0;
  t.mock.method(childProcess, 'spawn', (command, args, options) => {
    assert.deepEqual(args, ['editor-session']);
    assert.equal(options.cwd, directory);
    const child = new EventEmitter();
    const version = readFileSync(path.resolve(directory, command), 'utf8');
    const serial = ++starts;
    child.stdout = new PassThrough();
    child.stderr = new PassThrough();
    child.stdin = new Writable({ write(_bytes, _encoding, done) {
      setImmediate(() => child.stdout.write(JSON.stringify({ version, serial }) + '\n'));
      done();
    } });
    child.kill = () => { stops++; child.emit('close', null); };
    return child;
  });
  const workers = require('../editor-process');
  t.after(async () => {
    workers.close();
    if (previousPath === undefined) delete process.env.PATH;
    else process.env.PATH = previousPath;
    await fs.rm(directory, { recursive: true, force: true });
  });
  await fs.writeFile(executable, 'v1', { mode: 0o700 });
  const request = (command = executable, signal) => workers.request(command, directory, ['editor-check'], signal)
    .then(response => JSON.parse(response.stdout));
  assert.deepEqual(await request(), { version: 'v1', serial: 1 });
  assert.deepEqual(await request(), { version: 'v1', serial: 1 });

  // Atomic replacement retains size and mtime, but changes executable identity.
  const metadata = await fs.stat(executable);
  const replacement = path.join(directory, 'replacement');
  await fs.writeFile(replacement, 'v2', { mode: 0o700 });
  await fs.utimes(replacement, metadata.atime, metadata.mtime);
  await fs.rename(replacement, executable);
  assert.deepEqual(await Promise.all([request(), request()]), [
    { version: 'v2', serial: 2 }, { version: 'v2', serial: 2 },
  ]);
  assert.equal(stops, 1);

  const cancel = new AbortController();
  const pending = request(executable, cancel.signal);
  cancel.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  assert.deepEqual(await request(), { version: 'v2', serial: 3 });

  // Relative and PATH spellings are resolved in the package's environment.
  assert.equal((await request(`.${path.sep}${name}`)).version, 'v2');
  process.env.PATH = `${directory}${path.delimiter}${previousPath || ''}`;
  const bare = await request(name);
  assert.deepEqual(await request(name), bare);
  await fs.unlink(executable);
  await assert.rejects(request(), { code: 'ENOENT' });
});

'use strict';
const { spawn } = require('node:child_process');
const fs = require('node:fs/promises');
const { constants } = require('node:fs');
const path = require('node:path');

const workers = new Map();
const aborted = () => Object.assign(new Error('Editor request canceled'), { name: 'AbortError' });
const field = value => Buffer.concat([Buffer.from(`${Buffer.byteLength(value)}\n`), Buffer.from(value)]);

// A process-lifecycle freshness hint, not an authenticated artifact identity.
// Preserve the original spawn spelling so executable-relative tool discovery
// and the platform's own executable validation keep their existing behavior.
async function executableStamp(executable, directory) {
  const qualified = path.isAbsolute(executable) || /[/\\]/.test(executable);
  const search = qualified ? [path.resolve(directory, executable)] :
    (process.env.PATH ?? process.env.Path ?? '/usr/bin:/bin').split(path.delimiter)
      .map(folder => path.resolve(directory, folder, executable));
  if (!qualified && process.platform === 'win32') search.unshift(path.resolve(directory, executable));
  const extensions = process.platform === 'win32' && !path.extname(executable) ?
    (process.env.PATHEXT || '.COM;.EXE;.BAT;.CMD').split(';') : [''];
  for (const candidate of search) for (const extension of extensions) {
    try {
      const resolved = await fs.realpath(candidate + extension);
      const stat = await fs.stat(resolved, { bigint: true });
      if (!stat.isFile()) continue;
      await fs.access(resolved, constants.X_OK);
      return [resolved, stat.dev, stat.ino, stat.size, stat.mtimeNs, stat.ctimeNs].join(':');
    } catch { /* Let spawn report missing or inaccessible executables. */ }
  }
  return null;
}

// One bounded semantic snapshot per executable/package. Requests are serialized;
// canceling an active request kills only our worker, then restarts for the queue.
class Worker {
  constructor(executable, directory) {
    this.executable = executable;
    this.directory = directory;
    this.queue = [];
    this.child = null;
    this.current = null;
  }

  request(args, signal) {
    return new Promise((resolve, reject) => {
      if (signal?.aborted) { reject(aborted()); return; }
      const entry = { args, signal, resolve, reject };
      entry.cancel = () => {
        if (this.current === entry) this.fail(aborted());
        else {
          this.queue = this.queue.filter(value => value !== entry);
          signal?.removeEventListener('abort', entry.cancel);
          reject(aborted());
        }
      };
      signal?.addEventListener('abort', entry.cancel, { once: true });
      this.queue.push(entry);
      this.pump();
    });
  }

  async pump() {
    if (this.current || !this.queue.length) return;
    const entry = this.current = this.queue.shift();
    try {
      const stamp = await executableStamp(this.executable, this.directory);
      if (this.current !== entry) return; // Canceled while resolving the executable.
      if (this.child && (stamp === null || stamp !== this.stamp)) this.retire();
      this.stamp = stamp;
      this.dispatch();
    } catch (error) {
      if (this.current === entry) this.fail(error);
    }
  }

  dispatch() {
    if (!this.child) {
      const child = spawn(this.executable, ['editor-session'], { cwd: this.directory, windowsHide: true, stdio: ['pipe', 'pipe', 'pipe'] });
      this.child = child;
      this.output = '';
      this.errors = '';
      child.stdout.setEncoding('utf8');
      child.stderr.setEncoding('utf8');
      child.stderr.on('data', data => { if (this.child === child) this.errors = (this.errors + data).slice(-16384); });
      child.stdout.on('data', data => {
        if (this.child !== child) return;
        this.output += data;
        if (this.output.length > 16 * 1024 * 1024) { this.fail(new Error('Editor response exceeds 16 MiB')); return; }
        const newline = this.output.indexOf('\n');
        if (newline < 0) return;
        const stdout = this.output.slice(0, newline);
        this.output = this.output.slice(newline + 1);
        this.finish(null, { code: 0, stdout, stderr: this.errors });
      });
      child.on('error', error => { if (this.child === child) this.fail(error); });
      child.stdin.on('error', error => { if (this.child === child) this.fail(error); });
      child.on('close', code => {
        if (this.child !== child) return;
        this.fail(new Error(this.errors || `Editor session exited ${code}`));
      });
    }
    this.child.stdin.write(Buffer.concat([field(String(this.current.args.length)), ...this.current.args.map(field)]));
  }

  finish(error, result) {
    const entry = this.current;
    this.current = null;
    if (entry) {
      entry.signal?.removeEventListener('abort', entry.cancel);
      if (error) entry.reject(error); else entry.resolve(result);
    }
    this.pump();
  }

  fail(error) {
    this.retire();
    this.finish(error);
  }

  retire() {
    const child = this.child;
    this.child = null;
    child?.kill();
  }

  close() {
    for (const entry of this.queue.splice(0)) {
      entry.signal?.removeEventListener('abort', entry.cancel);
      entry.reject(aborted());
    }
    this.fail(aborted());
  }
}

function request(executable, directory, args, signal) {
  const key = JSON.stringify([executable, directory]);
  if (!workers.has(key)) workers.set(key, new Worker(executable, directory));
  return workers.get(key).request(args, signal);
}

function close() {
  for (const worker of workers.values()) worker.close();
  workers.clear();
}

function retain(directories) {
  for (const [key, worker] of workers) {
    if (!directories.has(worker.directory)) {
      worker.close();
      workers.delete(key);
    }
  }
}

process.once('exit', close);
module.exports = { request, close, retain };

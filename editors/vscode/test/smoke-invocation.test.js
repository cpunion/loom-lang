'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { spawnSync } = require('node:child_process');

test('LSP smoke rejects ignored positional toolchain arguments before starting a worker', () => {
  const result = spawnSync(process.execPath, [path.join(__dirname, 'smoke.js'), 'unexpected-compiler'], {
    env: { ...process.env, LOOM_EDITOR_COMPILER: path.join(__dirname, 'missing-compiler') },
    encoding: 'utf8', timeout: 5000,
  });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /does not accept arguments.*LOOM_EDITOR_COMPILER.*LOOM_EDITOR_STD/);
  assert.doesNotMatch(result.stdout, /LSP smoke toolchain:/);
});

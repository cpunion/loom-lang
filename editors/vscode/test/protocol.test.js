'use strict';
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { bytePosition, snapshots } = require('../compiler');
const { session } = require('./session');
const folder = path.join(__dirname, 'fixtures/project');
const file = path.join(folder, 'main.loom');
const pause = ms => new Promise(resolve => setTimeout(resolve, ms));

test('UTF-8 compiler spans become UTF-16 LSP positions', () => {
  const document = TextDocument.create('file:///unicode.loom', 'loom', 1, 'é😀\r\n名x');
  assert.deepEqual(bytePosition(document, 6), { line: 0, character: 3 });
  assert.deepEqual(bytePosition(document, 11), { line: 1, character: 1 });
});

test('overlay snapshots preserve bytes and remove only their private directory', async () => {
  const overlays = await snapshots([{ path: file, text: '// é😀\r\n' }]);
  const snapshot = overlays.args[2];
  try { assert.equal(await fs.readFile(snapshot, 'utf8'), '// é😀\r\n'); }
  finally { await overlays.dispose(); }
  await assert.rejects(fs.access(path.dirname(snapshot)), { code: 'ENOENT' });
});

test('LSP checks unsaved/new buffers, suppresses stale results, and formats without source writes', async t => {
  const saved = await fs.readFile(file, 'utf8');
  // Node resolves the fixture's editor-check/fmt scripts in the package cwd.
  const client = await session({ executable: process.execPath }, folder);
  t.after(() => client.close());
  assert.equal(client.initialized.capabilities.documentFormattingProvider, true);
  await client.open(file, 'fn main() { discard "é😀"\nBROKEN }\n');
  const first = await client.wait(file, 1);
  assert.equal(first.diagnostics[0].range.start.line, 1);
  assert.equal(first.diagnostics[0].range.start.character, 0);
  assert.equal(first.diagnostics[0].message, 'fixture diagnostic');

  await client.change(file, '// SLOW\nBROKEN', 2);
  await pause(550); // The slow compiler is live; the next edit cancels it.
  await client.change(file, 'fn main() {}', 3);
  assert.deepEqual((await client.wait(file, 3)).diagnostics, []);
  await pause(600);
  assert.equal(client.diagnostics.some(report => report.version === 2), false);

  const fresh = path.join(folder, 'new_file.loom');
  await client.open(fresh, 'BROKEN');
  assert.equal((await client.wait(fresh, 1)).diagnostics.length, 1);
  await assert.rejects(fs.access(fresh), { code: 'ENOENT' });
  await client.rpc.sendNotification('textDocument/didClose', { textDocument: { uri: URI.file(fresh).toString() } });

  const edits = await client.rpc.sendRequest('textDocument/formatting', { textDocument: { uri: URI.file(file).toString() }, options: { tabSize: 4, insertSpaces: true } });
  assert.equal(edits[0].newText, 'fn main() {\n    assert true\n}\n');
  await client.change(file, '// SLOW\nfn main() {}', 4);
  const stale = client.rpc.sendRequest('textDocument/formatting', { textDocument: { uri: URI.file(file).toString() }, options: { tabSize: 4, insertSpaces: true } });
  await pause(100);
  await client.change(file, 'fn main() {}', 5);
  assert.deepEqual(await stale, []);
  await client.change(file, 'BAD_FORMAT', 6);
  await assert.rejects(client.rpc.sendRequest('textDocument/formatting', { textDocument: { uri: URI.file(file).toString() }, options: { tabSize: 4, insertSpaces: true } }), /fixture formatting error/);
  assert.equal(await fs.readFile(file, 'utf8'), saved);
});

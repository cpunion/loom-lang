'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { session } = require('./session');

async function main() {
  const repository = path.resolve(__dirname, '../../..');
  const executable = process.env.LOOM_EDITOR_COMPILER || path.join(repository, 'target', process.platform === 'win32' ? 'loom.exe' : 'loom');
  const stdRoot = process.env.LOOM_EDITOR_STD || path.join(repository, 'compiler/std');
  const folder = path.join(__dirname, 'fixtures/project');
  const file = path.join(folder, 'main.loom');
  const saved = await fs.readFile(file, 'utf8');
  const client = await session({ executable, stdRoot }, folder);
  try {
    await client.open(file, 'fn main() { let value Int = true\ndiscard value }\n');
    assert.ok((await client.wait(file, 1)).diagnostics.length > 0);
    const helper = path.join(folder, 'overlay_helper.loom');
    await client.open(helper, 'fn helper() Int { 42 }\n');
    await client.change(file, 'fn main(){assert helper()==42}\n', 2);
    assert.deepEqual((await client.wait(file, 2)).diagnostics, []);
    const edits = await client.rpc.sendRequest('textDocument/formatting', { textDocument: { uri: URI.file(file).toString() }, options: { tabSize: 4, insertSpaces: true } });
    assert.equal(edits.length, 1);
    assert.match(edits[0].newText, /fn main\(\) \{/);
    assert.equal(await fs.readFile(file, 'utf8'), saved);
    await assert.rejects(fs.access(helper), { code: 'ENOENT' });
    console.log('Real compiler LSP smoke passed: unsaved diagnostics, new sibling overlay, formatting, no source writes.');
  } finally { await client.close(); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });

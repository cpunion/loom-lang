'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
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
    const helperText = 'fn helper() Int { 42 }\n';
    await client.open(helper, helperText);
    const source = 'fn main(){discard "é😀"\nlet value=helper()\nassert value==42}\n';
    await client.change(file, source, 2);
    assert.deepEqual((await client.wait(file, 2)).diagnostics, []);
    const params = { textDocument: { uri: URI.file(file).toString() }, position: { line: 2, character: 7 } };
    const hover = await client.rpc.sendRequest('textDocument/hover', params);
    assert.ok(hover.contents.some(item => item.value === 'Int'));
    const definitions = await client.rpc.sendRequest('textDocument/definition', { ...params, position: { line: 1, character: 11 } });
    assert.equal(definitions.length, 1);
    assert.equal(definitions[0].uri, URI.file(helper).toString());
    const helperDocument = TextDocument.create(definitions[0].uri, 'loom', 1, helperText);
    assert.match(helperDocument.getText(definitions[0].range), /helper/);
    const edits = await client.rpc.sendRequest('textDocument/formatting', { textDocument: { uri: URI.file(file).toString() }, options: { tabSize: 4, insertSpaces: true } });
    assert.equal(edits.length, 1);
    assert.match(edits[0].newText, /fn main\(\) \{/);
    await client.change(file, 'fn main(){break}\n', 3);
    assert.ok((await client.wait(file, 3)).diagnostics.some(item => item.message.includes('enclosing while body')));
    await client.change(file, 'fn main(){while true{if false{continue}\nbreak}}\n', 4);
    assert.deepEqual((await client.wait(file, 4)).diagnostics, []);
    const loops = await client.rpc.sendRequest('textDocument/formatting', { textDocument: { uri: URI.file(file).toString() }, options: { tabSize: 4, insertSpaces: true } });
    assert.match(loops[0].newText, /\n {12}continue\n/);
    assert.match(loops[0].newText, /\n {8}break\n/);
    assert.equal(await fs.readFile(file, 'utf8'), saved);
    await assert.rejects(fs.access(helper), { code: 'ENOENT' });
    console.log('Real compiler LSP smoke passed: unsaved diagnostics, sibling overlay, type hover, definition, loop scope/formatting, no source writes.');
  } finally { await client.close(); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });

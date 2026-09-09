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
  const folderUri = URI.file(folder).toString();
  const scoped = { executable: path.relative(folder, executable), stdRoot: path.relative(folder, stdRoot) };
  const client = await session({}, folder, uri =>
    uri === folderUri || uri.startsWith(folderUri + '/') ? scoped : { executable: './missing-external-toolchain' });
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
    await client.change(file, 'fn main(){let values=[1,2]\ndiscard values\nwhile true{if false{continue}\nbreak}}\n', 4);
    assert.deepEqual((await client.wait(file, 4)).diagnostics, []);
    const listHover = await client.rpc.sendRequest('textDocument/hover', { ...params, position: { line: 1, character: 9 } });
    assert.ok(listHover.contents.some(item => item.value === 'List[Int]'));
    const loops = await client.rpc.sendRequest('textDocument/formatting', { textDocument: { uri: URI.file(file).toString() }, options: { tabSize: 4, insertSpaces: true } });
    assert.match(loops[0].newText, /\n {12}continue\n/);
    assert.match(loops[0].newText, /\n {8}break\n/);
    assert.match(loops[0].newText, /let values = \[1, 2\]/);
    const indexed = 'fn main(){let values=[1,2]\nvalues[0]=true\nlet first=values[0]\nassert first==4}\n';
    await client.change(file, indexed, 5);
    assert.ok((await client.wait(file, 5)).diagnostics.length > 0);
    await client.change(file, indexed.replace('=true', '=4'), 6);
    assert.deepEqual((await client.wait(file, 6)).diagnostics, []);
    const receiver = await client.rpc.sendRequest('textDocument/hover', { ...params, position: { line: 2, character: 12 } });
    assert.ok(receiver.contents.some(item => item.value === 'List[Int]'));
    const element = await client.rpc.sendRequest('textDocument/hover', { ...params, position: { line: 3, character: 7 } });
    assert.ok(element.contents.some(item => item.value === 'Int'));
    const indexing = await client.rpc.sendRequest('textDocument/formatting', { textDocument: { uri: URI.file(file).toString() }, options: { tabSize: 4, insertSpaces: true } });
    assert.match(indexing[0].newText, /values\[0\] = 4/);
    assert.match(indexing[0].newText, /let first = values\[0\]/);

    const at = (target, text, fragment) => {
      const offset = text.indexOf(fragment);
      assert.notEqual(offset, -1);
      const uri = URI.file(target).toString();
      return { textDocument: { uri }, position: TextDocument.create(uri, 'loom', 0, text).positionAt(offset) };
    };
    const bad = path.join(folder, 'overlay_bad.loom');
    const broken = 'fn broken() Int { let n Int = true\nn }\n';
    await client.open(bad, broken);
    for (const sameFile of [false, true]) {
      if (sameFile) await client.rpc.sendNotification('textDocument/didClose', { textDocument: { uri: URI.file(bad).toString() } });
      const mixed = sameFile ? source + broken : source;
      await client.change(file, mixed, sameFile ? 8 : 7);
      assert.ok((await client.wait(sameFile ? file : bad, sameFile ? 8 : 1)).diagnostics.length > 0);
      const retained = await client.rpc.sendRequest('textDocument/hover', at(file, mixed, 'value=='));
      assert.ok(retained.contents.some(item => item.value === 'Int'));
      assert.deepEqual(await client.rpc.sendRequest('textDocument/definition', at(file, mixed, 'helper()')), definitions);
      const invalid = at(sameFile ? file : bad, sameFile ? mixed : broken, 'n }');
      assert.equal(await client.rpc.sendRequest('textDocument/hover', invalid), null);
      assert.deepEqual(await client.rpc.sendRequest('textDocument/definition', invalid), []);
    }
    // A declared return type is not a checked result when the callee fails.
    const dependent = source.replace('helper()', 'broken()') + broken;
    await client.change(file, dependent, 9);
    assert.ok((await client.wait(file, 9)).diagnostics.length > 0);
    assert.equal(await client.rpc.sendRequest('textDocument/hover', at(file, dependent, 'value==')), null);
    assert.deepEqual(await client.rpc.sendRequest('textDocument/definition', at(file, dependent, 'broken()')), []);
    const repaired = dependent.replace('Int = true', 'Int = 42');
    await client.change(file, repaired, 10);
    assert.deepEqual((await client.wait(file, 10)).diagnostics, []);
    const recovered = await client.rpc.sendRequest('textDocument/hover', at(file, repaired, 'value=='));
    assert.ok(recovered.contents.some(item => item.value === 'Int'));
    const recoveredDefinitions = await client.rpc.sendRequest('textDocument/definition', at(file, repaired, 'broken()'));
    assert.equal(recoveredDefinitions.length, 1);
    assert.equal(recoveredDefinitions[0].uri, URI.file(file).toString());
    assert.equal(TextDocument.create(recoveredDefinitions[0].uri, 'loom', 10, repaired).getText(recoveredDefinitions[0].range), 'broken');
    // Complete an unknown identifier in the middle of a token, despite body
    // errors, using the unsaved package (including its new helper file).
    const partial = 'fn main(){discard "é😀"\nlet count=42\nlet copied=coRest\n}\n';
    await client.change(file, partial, 11);
    assert.ok((await client.wait(file, 11)).diagnostics.length > 0);
    const partialDocument = TextDocument.create(params.textDocument.uri, 'loom', 11, partial);
    const completions = await client.rpc.sendRequest('textDocument/completion', {
      ...params, position: partialDocument.positionAt(partial.indexOf('coRest') + 2),
    });
    assert.equal(completions.isIncomplete, true);
    assert.deepEqual(completions.items.map(item => item.label), ['count']);
    assert.equal(partialDocument.getText(completions.items[0].textEdit.range), 'coRest');
    const completed = TextDocument.applyEdits(partialDocument, [completions.items[0].textEdit]);
    await client.change(file, completed, 12);
    assert.deepEqual((await client.wait(file, 12)).diagnostics, []);
    const imported = await client.rpc.sendRequest('textDocument/completion', at(file, completed, 'count=42'));
    assert.ok(imported.items.some(item => item.label === 'helper' && item.detail === 'fn helper() Int'));
    for (const [index, suffix] of ['co', ''].entries()) {
      const unfinished = 'fn main(){discard "é😀"\nlet count=42\nlet copied = ' + suffix;
      const version = 13 + index * 2;
      await client.change(file, unfinished, version);
      assert.ok((await client.wait(file, version)).diagnostics.length > 0);
      const snapshot = TextDocument.create(params.textDocument.uri, 'loom', version, unfinished);
      const recoveredNames = await client.rpc.sendRequest('textDocument/completion', {
        ...params, position: snapshot.positionAt(unfinished.length),
      });
      const count = recoveredNames.items.find(item => item.label === 'count');
      assert.ok(count);
      assert.equal(snapshot.getText(count.textEdit.range), suffix);
      assert.deepEqual(count.textEdit.range.end, snapshot.positionAt(unfinished.length));
      // Completion does not clear the original syntax diagnostic or insert a
      // synthetic '_' / closing delimiter into the user's buffer.
      assert.ok(client.diagnostics.filter(report => report.uri === params.textDocument.uri).at(-1).diagnostics.length > 0);
      const accepted = TextDocument.applyEdits(snapshot, [count.textEdit]) + '\n}\n';
      await client.change(file, accepted, version + 1);
      assert.deepEqual((await client.wait(file, version + 1)).diagnostics, []);
    }
    const memberCases = [
      { text: 'record Receipt { amount Int }\nfn main(){discard "é😀"\nlet receipt=Receipt { amount=42 }\ndiscard receipt.amRest', prefix: 'am', suffix: 'amRest', name: 'amount', kind: 5 },
      { text: 'record Receipt { amount Int }\nfn main(){let receipt=Receipt { amount=42 }\ndiscard receipt.', prefix: '', suffix: '', name: 'amount', kind: 5 },
      { text: 'concept Show { fn show(self Self) Int }\nimpl Show for Int { fn show(self Int) Int { self } }\nfn main(){let value=42\ndiscard value.sh', prefix: 'sh', suffix: 'sh', name: 'show', kind: 2, call: '()' },
      { text: 'async fn item() Int { 42 }\nasync fn main(){let pending=item()\ndiscard pending.aw', prefix: 'aw', suffix: 'aw', name: 'await', kind: 14 },
      { text: 'import std.text.length\nfn main(){discard std.text.leRest', prefix: 'le', suffix: 'leRest', name: 'length', kind: 3, call: '("é")' },
      { text: 'import std.text.length\nfn main(){discard std.teRest', prefix: 'te', suffix: 'teRest', name: 'text', kind: 9, call: '.length("é")' },
    ];
    for (const [index, item] of memberCases.entries()) {
      const version = 17 + index * 2;
      await client.change(file, item.text, version);
      assert.ok((await client.wait(file, version)).diagnostics.length > 0);
      const snapshot = TextDocument.create(params.textDocument.uri, 'loom', version, item.text);
      const offset = item.text.length - item.suffix.length + item.prefix.length;
      const members = await client.rpc.sendRequest('textDocument/completion', {
        ...params, position: snapshot.positionAt(offset),
      });
      assert.deepEqual(members.items.map(value => value.label), [item.name]);
      const selected = members.items[0];
      assert.equal(selected.kind, item.kind);
      assert.equal(snapshot.getText(selected.textEdit.range), item.suffix);
      const accepted = TextDocument.applyEdits(snapshot, [selected.textEdit]) + (item.call || '') + '\n}\n';
      await client.change(file, accepted, version + 1);
      assert.deepEqual((await client.wait(file, version + 1)).diagnostics, []);
    }
    const annotated = 'import std.option.Option\nfn demo(value std.option.OpRest[Int]) {}\nfn main() {}\n';
    await client.change(file, annotated, 31);
    assert.ok((await client.wait(file, 31)).diagnostics.length > 0);
    const annotation = TextDocument.create(params.textDocument.uri, 'loom', 31, annotated);
    const types = await client.rpc.sendRequest('textDocument/completion', {
      ...params, position: annotation.positionAt(annotated.indexOf('OpRest') + 2),
    });
    assert.deepEqual(types.items.map(item => item.label), ['Option']);
    assert.equal(types.items[0].kind, 7);
    assert.equal(annotation.getText(types.items[0].textEdit.range), 'OpRest');
    await client.change(file, TextDocument.applyEdits(annotation, [types.items[0].textEdit]), 32);
    assert.deepEqual((await client.wait(file, 32)).diagnostics, []);
    const importCases = [
      { text: 'import std.teRest\nfn main() {}\n', suffix: 'teRest', prefix: 'te', name: 'text', kind: 9, append: '.length' },
      { text: 'import std.text.leRest\nfn main() { assert length("é😀") == 6 }\n', suffix: 'leRest', prefix: 'le', name: 'length', kind: 3 },
      { text: 'import std.option.\nfn main() {}\n', suffix: '', prefix: '', name: 'Option', kind: 7 },
    ];
    for (const [index, item] of importCases.entries()) {
      const version = 33 + index * 2;
      await client.change(file, item.text, version);
      assert.ok((await client.wait(file, version)).diagnostics.length > 0);
      const snapshot = TextDocument.create(params.textDocument.uri, 'loom', version, item.text);
      const offset = item.text.indexOf('\n') - item.suffix.length + item.prefix.length;
      const imports = await client.rpc.sendRequest('textDocument/completion', { ...params, position: snapshot.positionAt(offset) });
      const selected = imports.items.find(value => value.label === item.name);
      assert.ok(selected);
      assert.equal(selected.kind, item.kind);
      assert.equal(snapshot.getText(selected.textEdit.range), item.suffix);
      const accepted = TextDocument.applyEdits(snapshot, [{ ...selected.textEdit, newText: selected.textEdit.newText + (item.append || '') }]);
      await client.change(file, accepted, version + 1);
      assert.deepEqual((await client.wait(file, version + 1)).diagnostics, []);
    }
    // Following a dependency outside this folder retains its folder-scoped,
    // relative toolchain settings for diagnostics, hover and formatting.
    const external = path.join(stdRoot, 'loom/source/source.loom');
    const externalText = await fs.readFile(external, 'utf8');
    await client.open(external, externalText);
    assert.deepEqual((await client.wait(external, 1)).diagnostics, []);
    const externalHover = await client.rpc.sendRequest('textDocument/hover', at(external, externalText, 'current == 13'));
    assert.ok(externalHover.contents.some(item => item.value === 'Int'));
    assert.ok(Array.isArray(await client.rpc.sendRequest('textDocument/formatting', {
      textDocument: { uri: URI.file(external).toString() }, options: { tabSize: 4, insertSpaces: true },
    })));
    assert.equal(await fs.readFile(external, 'utf8'), externalText);
    assert.equal(await fs.readFile(file, 'utf8'), saved);
    await assert.rejects(fs.access(helper), { code: 'ENOENT' });
    await assert.rejects(fs.access(bad), { code: 'ENOENT' });
    console.log('Real compiler LSP smoke passed: unsaved diagnostics, overlays, checked hover/definition, name/member/qualified/import completion and replacement, dependency rejection and repair, loops, indexing, external-file toolchain settings, formatting, no source writes.');
  } finally { await client.close(); }
}
main().catch(error => { console.error(error); process.exitCode = 1; });

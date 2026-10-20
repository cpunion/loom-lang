'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');

async function typeAliasRenameSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-type-alias-rename-'));
  const folder = await fs.realpath(temporary), file = path.join(folder, 'main.loom');
  const uri = URI.file(file).toString();
  const client = await session({ executable, stdRoot }, folder);
  const source = `concept Family {
    type Item[U]
}

fn identity_type[T]() type {
    T
}

concept Pass {
    type Out
}

impl[Ts...] Pass for (Ts...) {
    type Out = dyn Family[Item[U] = (comptime {
        let Whole = Self
        identity_type[Whole]()
    }, U)]
}

record Box {
    value Int
}

fn main() {
    let Chosen = Box
    let made = Chosen { value = 7 }
    {
        let Chosen = Bool
        let flag Chosen = true
        discard flag
    }
    let another Chosen = made
    discard another
}
`;
  let version = 1;
  try {
    await client.open(file, source, version);
    assert.deepEqual((await client.wait(file, version)).diagnostics, []);
    const document = TextDocument.create(uri, 'loom', version, source);
    for (const [fragment, declaration, count, conflict] of [
      ['Whole]()', 'Whole = Self', 2, 'identity_type'],
      ['Chosen {', 'Chosen = Box', 3, 'made'],
      ['Chosen = Bool', 'Chosen = Bool', 2, 'flag'],
    ]) {
      const params = { textDocument: { uri }, position: document.positionAt(source.indexOf(fragment)) };
      const definitions = await client.rpc.sendRequest('textDocument/definition', params);
      assert.equal(definitions.length, 1, fragment);
      assert.deepEqual(definitions[0].range.start, document.positionAt(source.indexOf(declaration)), fragment);
      const references = await client.rpc.sendRequest('textDocument/references', {
        ...params, context: { includeDeclaration: true },
      });
      assert.equal(references.length, count, fragment);
      const edits = await client.rpc.sendRequest('textDocument/rename', { ...params, newName: 'Renamed' });
      assert.equal(edits.changes[uri].length, count, fragment);
      assert.ok(edits.changes[uri].every(edit => document.getText(edit.range) === declaration.split(' ')[0]));
      await client.change(file, TextDocument.applyEdits(document, edits.changes[uri]), ++version);
      assert.deepEqual((await client.wait(file, version)).diagnostics, [], fragment);
      await client.change(file, source, ++version);
      assert.deepEqual((await client.wait(file, version)).diagnostics, []);
      await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...params, newName: conflict }),
        /conflict|invalidate/);
    }
    const unselected = source.replace('identity_type[Whole]()',
      'comptime if true { identity_type[Whole]() } else { identity_type[Whole]() }');
    await client.change(file, unselected, ++version);
    assert.deepEqual((await client.wait(file, version)).diagnostics, []);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', {
      textDocument: { uri }, position: document.positionAt(source.indexOf('Whole = Self')), newName: 'Renamed',
    }), /every occurrence/);
    await client.change(file, source + '\nfn broken() Int { false }\n', ++version);
    assert.equal((await client.wait(file, version)).diagnostics.length, 1);
    const params = { textDocument: { uri }, position: document.positionAt(source.indexOf('Whole]()')) };
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...params, newName: 'Renamed' }),
      /Rename requires a checked package: expected Int, found Bool/);
    await assert.rejects(fs.access(file), { code: 'ENOENT' });
    console.log('Type alias rename passed: CTFE headers, erased constructors, lexical shadows and checked-package failures.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

module.exports = { typeAliasRenameSmoke };

'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');
const { methodReferenceSource } = require('./method-references');

async function methodRenameSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-method-rename-'));
  const folder = await fs.realpath(temporary);
  const file = path.join(folder, 'main.loom'), test = path.join(folder, 'read_test.loom');
  const uri = URI.file(file).toString(), testUri = URI.file(test).toString();
  const source = methodReferenceSource().replace('concept Read {', `concept Read {
    fn relay(self Self) Int {
        self.read(23)
    }`).replace('fn main() {', `fn main() {
    assert true.relay() == 23
    let text = "other"
    assert text.read(4) == 4`) + `
impl[Ts...] Read for (Ts...) {
    fn read(self (Ts...), value Int) Int {
        value
    }
    fn read(self (Ts...), value Bool) Bool {
        value
    }
}

concept Alternative {
    fn read(self Self, value Int) Int
}

impl Alternative for Text {
    fn read(self Text, value Int) Int {
        value
    }
}
`;
  const testSource = `test fn checked_method() {
    assert true.read(12) == 12
}
`;
  await fs.writeFile(file, source);
  await fs.writeFile(test, testSource);
  const client = await session({ executable, stdRoot }, folder);
  try {
    await client.open(file, source);
    assert.deepEqual((await client.wait(file, 1)).diagnostics, []);
    await client.open(test, testSource);
    assert.deepEqual((await client.wait(test, 1)).diagnostics, []);
    const document = TextDocument.create(uri, 'loom', 1, source);
    const testDocument = TextDocument.create(testUri, 'loom', 1, testSource);
    const offsets = [
      'read(self Self, value Int)', 'read(self Bool, value Int)',
      'read(self Int, value Int)', 'read(self (Ts...), value Int)',
      'read(11)', 'read\n', 'read(7)', 'read(8)', 'read(9)',
      'read(23)',
    ].map(fragment => {
      const offset = source.indexOf(fragment);
      assert.notEqual(offset, -1, fragment);
      return offset;
    });
    const rename = (offset, newName = 'fetch') => client.rpc.sendRequest('textDocument/rename', {
      textDocument: { uri }, position: document.positionAt(offset), newName,
    });
    let edits;
    for (const offset of offsets) {
      const current = await rename(offset);
      assert.deepEqual(Object.keys(current.changes).sort(), [uri, testUri].sort());
      assert.equal(current.changes[uri].length, 10);
      assert.equal(current.changes[testUri].length, 1);
      assert.ok(current.changes[uri].every(edit => document.getText(edit.range) === 'read'));
      assert.ok(current.changes[testUri].every(edit => testDocument.getText(edit.range) === 'read'));
      if (edits) assert.deepEqual(current, edits);
      edits = current;
    }
    assert.deepEqual(await client.rpc.sendRequest('textDocument/rename', {
      textDocument: { uri: testUri },
      position: testDocument.positionAt(testSource.indexOf('read(12)')), newName: 'fetch',
    }), edits);
    await assert.rejects(rename(offsets[0], 'main'), /conflict/);
    const changed = TextDocument.applyEdits(document, edits.changes[uri]);
    const changedTest = TextDocument.applyEdits(testDocument, edits.changes[testUri]);
    assert.ok(changed.includes('fn read(self Bool, value Bool)'));
    assert.ok(changed.includes('fn read(self Self) Text'));
    assert.ok(changed.includes('true.read(false)') && changed.includes('true.read()'));
    assert.ok(changed.includes('text.read(4)') && changed.includes('fn read(self Text, value Int)'));
    await client.change(test, changedTest, 2);
    // Both open buffers participate in the same virtual edit, without saving.
    await client.change(file, changed, 2);
    assert.deepEqual((await client.wait(file, 2)).diagnostics, []);
    assert.equal(await fs.readFile(file, 'utf8'), source);
    assert.equal(await fs.readFile(test, 'utf8'), testSource);

    await client.change(test, testSource, 3);
    await client.change(file, `${source}
fn unobserved() Int {
    comptime if true {
        0
    } else {
        true.read(99)
    }
}
`, 3);
    assert.deepEqual((await client.wait(file, 3)).diagnostics, []);
    await assert.rejects(rename(offsets[0]), /every occurrence/);
    await client.change(file, source.replace('concept Read {', 'pub concept Read {'), 4);
    assert.deepEqual((await client.wait(file, 4)).diagnostics, []);
    await assert.rejects(rename(offsets[0]), /private concept/);
    const staticSource = `${source}
concept Factory {
    fn make(value Int) Self
}

impl Factory for Bool {
    fn make(value Int) Bool {
        value > 0
    }
}

fn factories() Bool {
    let action fn(Int) Bool = Factory.make[Bool]
    action(7) && Factory.make[Bool](8)
}
`;
    await client.change(file, staticSource, 5);
    assert.deepEqual((await client.wait(file, 5)).diagnostics, []);
    const staticDocument = TextDocument.create(uri, 'loom', 5, staticSource);
    const staticEdit = await client.rpc.sendRequest('textDocument/rename', {
      textDocument: { uri }, position: staticDocument.positionAt(staticSource.indexOf('make(value Int) Self')),
      newName: 'manufacture',
    });
    assert.deepEqual(Object.keys(staticEdit.changes), [uri]);
    assert.equal(staticEdit.changes[uri].length, 4);
    assert.ok(staticEdit.changes[uri].every(edit => staticDocument.getText(edit.range) === 'make'));
    await client.change(file, TextDocument.applyEdits(staticDocument, staticEdit.changes[uri]), 6);
    assert.deepEqual((await client.wait(file, 6)).diagnostics, []);
    assert.equal(await fs.readFile(file, 'utf8'), source);
    console.log('Method rename passed: one checked private slot, overload separation, unused impl words, generic/dyn/bound calls, tests, overlays and incomplete/public refusal.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

module.exports = { methodRenameSmoke };

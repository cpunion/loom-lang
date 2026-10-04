'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');

async function conceptRenameSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-concept-rename-'));
  const folder = await fs.realpath(temporary);
  const sources = new Map([
    ['loom.toml', "[module]\nname = 'demo'\n"],
    ['library/main.loom', `pub concept Read {
    type Item
    fn read(self Self) Self.Item
}
impl Read for Int {
    type Item = Int
    fn read(self Int) Int {
        self
    }
}
concept Nested {
    type Source Read
}
record Box[T Read] {
    value T
}
fn uncalled[T Read](value T) T.Read.Item {
    Read.read(value)
}
pub fn optional[T](value T) Int {
    comptime if T implements Read {
        discard Read.read(value)
        1
    } else {
        0
    }
}
test fn embedded() {
    let value dyn Read[Item = Int] = 7
    assert value.read() == 7
    assert optional(7) == 1
}
`],
    ['library/main_test.loom', `test fn own_test() {
    assert Read.read(8) == 8
}
`],
    ['app/main.loom', `// é😀 Read stays in comments and strings.
import demo.library.Read
import demo.library.optional

fn project[T Read](value T) T.demo.library.Read.Item {
    let Receiver = comptime { T }
    let Item = comptime { Receiver.demo.library.Read.Item }
    let selected Item = demo.library.Read.read(value)
    selected
}
fn main() {
    let value dyn demo.library.Read[Item = Int] = 7
    assert value.read() == 7
    assert Read.read(5) == 5
    assert optional(7) == 1
    assert project(9) == 9
    let Read = 8
    assert Read.read() == 8
    assert "Read" == "Read"
}
`],
    ['app/main_test.loom', `test fn app_test() {
    let value dyn Read[Item = Int] = 9
    assert value.read() == 9
}
`],
    ['other/main.loom', `pub concept Read {
    fn unrelated(self Self) Bool
}
`],
    ['private/main.loom', `concept Secret {
    fn value(self Self) Int
}
impl Secret for Int {
    fn value(self Int) Int {
        self
    }
}
fn main() {
    let item dyn Secret = 4
    assert item.value() == 4
}
`],
    ['private/main_test.loom', `test fn secret_test() {
    assert Secret.value(8) == 8
}
`],
  ]);
  for (const [name, text] of sources) {
    const file = path.join(folder, name);
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, text);
  }
  const app = path.join(folder, 'app/main.loom'), library = path.join(folder, 'library/main.loom');
  const source = sources.get('app/main.loom'), original = sources.get('library/main.loom');
  const client = await session({ executable, stdRoot }, folder);
  const at = (file, text, fragment) => {
    assert.ok(text.includes(fragment), fragment);
    return {
      textDocument: { uri: URI.file(file).toString() },
      position: TextDocument.create(URI.file(file).toString(), 'loom', 1, text).positionAt(text.indexOf(fragment)),
    };
  };
  const selected = at(app, source, 'Read[Item');
  const rename = () => client.rpc.sendRequest('textDocument/rename', { ...selected, newName: 'Reader' });
  try {
    await client.open(app, source);
    await client.open(library, original);
    assert.deepEqual((await client.wait(app, 1)).diagnostics, []);
    assert.deepEqual((await client.wait(library, 1)).diagnostics, []);
    const definitions = await client.rpc.sendRequest('textDocument/definition', selected);
    assert.equal(definitions.length, 1, JSON.stringify(definitions));
    assert.equal(definitions[0].uri, URI.file(library).toString());
    for (const fragment of ['Read\n', 'Read](value', 'Read.Item', 'Read.Item }', 'Read.read(value)', 'Read.read(5)']) {
      assert.deepEqual(await client.rpc.sendRequest('textDocument/definition', at(app, source, fragment)), definitions);
    }
    const members = await client.rpc.sendRequest('textDocument/definition', at(app, source, 'Item }'));
    assert.equal(members.length, 1, JSON.stringify(members));
    assert.equal(members[0].uri, URI.file(library).toString());
    const libraryDocument = TextDocument.create(URI.file(library).toString(), 'loom', 1, original);
    const memberStart = original.indexOf('Item\n');
    assert.deepEqual(members[0].range, {
      start: libraryDocument.positionAt(memberStart), end: libraryDocument.positionAt(memberStart + 4),
    });
    assert.deepEqual(members, await client.rpc.sendRequest('textDocument/definition', at(library, original, 'Item\n}')));
    const references = await client.rpc.sendRequest('textDocument/references', {
      ...selected, context: { includeDeclaration: true },
    });
    assert.equal(references.length, 19, JSON.stringify(references));
    const edit = await rename();
    assert.equal(Object.values(edit.changes).flat().length, 19, JSON.stringify(edit));
    assert.deepEqual(Object.keys(edit.changes).sort(), ['library/main.loom', 'library/main_test.loom', 'app/main.loom', 'app/main_test.loom']
      .map(name => URI.file(path.join(folder, name)).toString()).sort());
    const changed = TextDocument.applyEdits(TextDocument.create(URI.file(app).toString(), 'loom', 1, source), edit.changes[URI.file(app).toString()]);
    assert.ok(changed.includes('let Read = 8') && changed.includes('Read.read()'));
    assert.ok(changed.includes('// é😀 Read') && changed.includes('"Read"'));
    assert.ok(changed.includes('comptime { Receiver.demo.library.Reader.Item }'));
    assert.deepEqual(await client.rpc.sendRequest('textDocument/rename', {
      ...at(library, original, 'Read {'), newName: 'Reader',
    }), edit);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...selected, newName: 'Nested' }), /conflict/);

    // An unsaved occurrence in an unopened-on-disk test joins the same edit.
    const testFile = path.join(folder, 'library/main_test.loom');
    const testOverlay = `${sources.get('library/main_test.loom')}test fn unsaved() {\n    assert Read.read(10) == 10\n}\n`;
    await client.open(testFile, testOverlay);
    assert.equal(Object.values((await rename()).changes).flat().length, 20);
    await client.change(testFile, sources.get('library/main_test.loom'), 2);

    // Unvisited branches cannot supply binding evidence, even if editing them
    // would leave the selected build successful. The rename must be complete.
    await client.change(library, `${original}\nfn hidden() {\n    comptime if false {\n        discard Read.read(1)\n    }\n}\n`, 2);
    await assert.rejects(rename(), /every occurrence/);
    await client.change(library, original, 3);
    assert.deepEqual(await rename(), edit);

    const privateFile = path.join(folder, 'private/main.loom'), privateSource = sources.get('private/main.loom');
    await client.open(privateFile, privateSource);
    const privateEdit = await client.rpc.sendRequest('textDocument/rename', {
      ...at(privateFile, privateSource, 'Secret {'), newName: 'Internal',
    });
    assert.equal(Object.values(privateEdit.changes).flat().length, 4);
    assert.deepEqual(Object.keys(privateEdit.changes).sort(), ['private/main.loom', 'private/main_test.loom']
      .map(name => URI.file(path.join(folder, name)).toString()).sort());
    for (const [name, text] of sources) assert.equal(await fs.readFile(path.join(folder, name), 'utf8'), text);
    console.log('Concept rename passed: module imports/tests, bounds, impl, dyn, projections, qualified calls, overlays, shadows and incomplete-use refusal.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}
module.exports = { conceptRenameSmoke };

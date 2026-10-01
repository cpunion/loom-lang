'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');
const run = require('node:util').promisify(require('node:child_process').execFile);

async function fieldRenameSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-field-rename-'));
  const folder = await fs.realpath(temporary);
  const sources = new Map([
    ['loom.toml', "[module]\nname = 'demo'\n"],
    ['library/main.loom', `pub record Counter {
    count Int
    limit Int
}
pub type Valid = Counter where self.count >= 0 && self.count <= self.limit
pub fn identity(value Counter) Counter
ensures result.count == value.count
{
    value
}
pub fn read(value Valid) Int {
    let raw Counter = value
    let Counter { count = amount, .. } = raw
    amount
}
test fn embedded() {
    assert Counter { count = 4, limit = 9 }.count == 4
}
`],
    ['library/main_test.loom', `test fn library_test() {
    assert Counter { count = 5, limit = 9 }.count == 5
}
`],
    ['app/main.loom', `// é😀 count stays in comments and strings.
import demo.library.Counter
import demo.library.identity

record Other {
    count Int
}
fn main() {
    let count = 7
    let value = Counter { count = count, limit = 9 }
    let copy = Counter { count = value.count, ..value }
    let Counter { count = amount, .. } = identity(copy)
    assert amount == count && copy.count == 7
    assert Other { count = copy.count }.count == 7
    assert "count" == "count"
}
`],
    ['app/main_test.loom', `test fn app_test() {
    assert Counter { count = 8, limit = 9 }.count == 8
}
`],
    ['private/main.loom', `record Hidden {
    count Int
}
fn main() {
    let count = 7
    assert Hidden { count = count }.count == count
}
`],
    ['private/main_test.loom', `test fn private_test() {
    assert Hidden { count = 8 }.count == 8
}
`],
    ['unrelated/main.loom', `pub fn ordinary(count Int) Int {
    count
}
`],
  ]);
  for (const [name, text] of sources) {
    const file = path.join(folder, name);
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, text);
  }
  const library = path.join(folder, 'library/main.loom'), app = path.join(folder, 'app/main.loom');
  const original = sources.get('library/main.loom'), source = sources.get('app/main.loom');
  const client = await session({ executable, stdRoot }, folder);
  const at = (file, text, fragment) => ({
    textDocument: { uri: URI.file(file).toString() },
    position: TextDocument.create(URI.file(file).toString(), 'loom', 1, text).positionAt(text.indexOf(fragment)),
  });
  const selected = at(app, source, 'count = amount');
  try {
    await client.open(app, source);
    await client.open(library, original);
    assert.deepEqual((await client.wait(app, 1)).diagnostics, []);
    const edit = await client.rpc.sendRequest('textDocument/rename', { ...selected, newName: 'quantity' });
    assert.equal(Object.values(edit.changes).flat().length, 18, JSON.stringify(edit));
    assert.deepEqual(Object.keys(edit.changes).sort(), ['app/main.loom', 'app/main_test.loom', 'library/main.loom', 'library/main_test.loom']
      .map(name => URI.file(path.join(folder, name)).toString()).sort());
    assert.deepEqual(await client.rpc.sendRequest('textDocument/rename', {
      ...at(library, original, 'count Int'), newName: 'quantity',
    }), edit);
    const uri = URI.file(app).toString();
    const changed = TextDocument.applyEdits(TextDocument.create(uri, 'loom', 1, source), edit.changes[uri]);
    assert.ok(changed.includes('Counter { quantity = count, limit'));
    assert.ok(changed.includes('Other { count = copy.quantity }.count'));
    assert.ok(changed.includes('let count = 7') && changed.includes('// é😀 count') && changed.includes('"count"'));
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...selected, newName: 'limit' }), /conflict/);

    const unrelated = path.join(folder, 'unrelated/main.loom');
    // This package has not imported Counter. An unchecked structural access
    // still blocks the edit rather than falling outside the coverage audit.
    await client.open(unrelated, `${sources.get('unrelated/main.loom')}\nfn deferred[T](value T) Int {\n    comptime if T == Int {\n        value\n    } else {\n        value.count\n    }\n}\n`);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...selected, newName: 'quantity' }), /every occurrence/);
    await client.change(unrelated, sources.get('unrelated/main.loom'), 2);

    // A constructor label in an unselected template is a possible use, not a
    // declaration that may be silently skipped during the coverage audit.
    const hidden = `${original}\nfn hidden[T](value T) Counter {\n    discard value\n    comptime if T == Int {\n        Counter { count = 1, limit = 9 }\n    } else {\n        Counter { count = 2, limit = 9 }\n    }\n}\n`;
    await client.change(library, hidden, 2);
    assert.deepEqual((await client.wait(library, 2)).diagnostics, []);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...selected, newName: 'quantity' }), /every occurrence/);
    await client.change(library, original, 3);

    const privateFile = path.join(folder, 'private/main.loom'), privateSource = sources.get('private/main.loom');
    await client.open(privateFile, privateSource);
    const privateEdit = await client.rpc.sendRequest('textDocument/rename', {
      ...at(privateFile, privateSource, 'count Int'), newName: 'quantity',
    });
    assert.equal(Object.values(privateEdit.changes).flat().length, 5);
    assert.deepEqual(Object.keys(privateEdit.changes).sort(), ['private/main.loom', 'private/main_test.loom']
      .map(name => URI.file(path.join(folder, name)).toString()).sort());
    const tree = `${source.replace('fn main() {', `fn main() {
    let wrong Int = true
    discard wrong`)}
record Tree {
    children List[Tree]
}
`;
    await client.change(app, tree, 2);
    assert.ok((await client.wait(app, 2)).diagnostics.length > 0);
    const recursive = await client.rpc.sendRequest('textDocument/definition', at(app, tree, 'Tree]'));
    assert.equal(recursive.length, 1);
    const inline = tree.replace('children List[Tree]', 'children Tree');
    await client.change(app, inline, 3);
    assert.deepEqual(await client.rpc.sendRequest('textDocument/definition', at(app, inline, 'Tree\n}')), []);
    await client.change(app, source, 4);
    for (const [name, text] of sources) assert.equal(await fs.readFile(path.join(folder, name), 'utf8'), text);
    // Apply the exact returned edit only in this temporary project, then run
    // the ordinary compiler/test/native pipeline on the resulting sources.
    for (const [uri, edits] of Object.entries(edit.changes)) {
      const file = URI.parse(uri).fsPath, text = await fs.readFile(file, 'utf8');
      await fs.writeFile(file, TextDocument.applyEdits(TextDocument.create(uri, 'loom', 1, text), edits));
    }
    for (const command of ['check', 'test', 'run']) {
      await run(executable, [command, path.dirname(app), '--std', stdRoot]);
    }
    await run(executable, ['test', path.dirname(library), '--std', stdRoot]);
    console.log('Record field rename passed: contracts, refinements, updates, destructuring, independent packages/tests and distinct owners.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}
module.exports = { fieldRenameSmoke };

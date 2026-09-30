'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');

async function publicRenameSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-public-rename-'));
  const folder = await fs.realpath(temporary);
  const sources = new Map([
    ['loom.toml', "[module]\nname = 'demo'\n"],
    ['library/main.loom', `pub fn answer(value Int) Int
ensures result == value
{
    value
}
test fn embedded() {
    assert answer(4) == 4
}
`],
    ['library/main_test.loom', `test fn library_test() {
    assert answer(5) == 5
}
`],
    ['app/main.loom', `// é😀 answer is not a source reference.
import demo.library.answer

fn main() {
    let callback fn(Int) Int = answer
    assert callback(7) == demo.library.answer(7)
    assert "answer" == "answer"
}
`],
    ['app/main_test.loom', `test fn app_test() {
    assert answer(8) == 8
}
`],
    ['other/main.loom', `pub fn answer() Int {
    99
}
test fn other_test() {
    assert answer() == 99
}
`],
    ['nested/loom.toml', "[module]\nname = 'nested'\n"],
    ['nested/main.loom', 'not valid source; a separate module\n'],
  ]);
  for (const [name, text] of sources) {
    const file = path.join(folder, name);
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, text);
  }
  const app = path.join(folder, 'app/main.loom'), library = path.join(folder, 'library/main.loom');
  const uri = URI.file(app).toString();
  const source = sources.get('app/main.loom');
  const client = await session({ executable, stdRoot }, folder);
  const at = (file, text, name, last = false) => ({
    textDocument: { uri: URI.file(file).toString() },
    position: TextDocument.create(URI.file(file).toString(), 'loom', 1, text)
      .positionAt(last ? text.lastIndexOf(name) : text.indexOf(name)),
  });
  const params = at(app, source, 'answer', true);
  try {
    await client.open(app, source);
    await client.open(library, sources.get('library/main.loom'));
    assert.deepEqual((await client.wait(app, 1)).diagnostics, []);
    // Select the import's final segment, not a same-spelled comment or string.
    const imported = at(app, source, 'answer\n');
    const references = await client.rpc.sendRequest('textDocument/references', { ...imported, context: { includeDeclaration: true } });
    assert.equal(references.length, 7, JSON.stringify(references));
    const edit = await client.rpc.sendRequest('textDocument/rename', { ...imported, newName: 'identity' });
    const expected = ['app/main.loom', 'app/main_test.loom', 'library/main.loom', 'library/main_test.loom'];
    assert.deepEqual(Object.keys(edit.changes).sort(), expected.map(name => URI.file(path.join(folder, name)).toString()).sort());
    assert.equal(references.length, Object.values(edit.changes).flat().length);
    assert.equal(edit.changes[uri].length, 3);
    const fromDeclaration = await client.rpc.sendRequest('textDocument/rename', {
      ...at(library, sources.get('library/main.loom'), 'answer'), newName: 'identity',
    });
    assert.deepEqual(fromDeclaration, edit);
    const revised = TextDocument.applyEdits(TextDocument.create(uri, 'loom', 1, source), edit.changes[uri]);
    assert.ok(revised.includes('demo.library.identity(7)'));
    assert.ok(revised.includes('// é😀 answer') && revised.includes('"answer"'));

    // A new, unsaved test-only directory is still part of the module selection.
    const testDirectory = path.join(folder, 'trial');
    await fs.mkdir(testDirectory);
    const testFile = path.join(testDirectory, 'unsaved_test.loom');
    await client.open(testFile, 'import demo.library.answer\n\ntest fn trial() {\n    assert answer(3) == 3\n}\n');
    const unsaved = await client.rpc.sendRequest('textDocument/rename', { ...imported, newName: 'identity' });
    assert.equal(unsaved.changes[URI.file(testFile).toString()].length, 2);
    await assert.rejects(fs.access(testFile), { code: 'ENOENT' });

    const hidden = `${sources.get('library/main.loom')}\nfn unused[T](value T) Int {\n    answer(2)\n}\n`;
    await client.change(library, hidden, 2);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...imported, newName: 'identity' }), /every occurrence/);
    await client.change(library, sources.get('library/main.loom'), 3);

    const generated = `import std.reflect.Schema
${sources.get('library/main.loom')}
fn generated(types List[Schema]) Text {
    "answer(1)"
}
test fn generated_test() {
    assert generated!() == 1
}
`;
    await client.change(library, generated, 4);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...imported, newName: 'identity' }), /generated source references/);
    await client.change(library, `${sources.get('library/main.loom')}\npub fn answer(value Bool) Bool {\n    value\n}\n`, 5);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', {
      ...at(library, sources.get('library/main.loom'), 'answer'), newName: 'identity',
    }), /one top-level function/);
    await client.change(library, sources.get('library/main.loom'), 6);

    // Rechecking alone could accept accidental capture by this callback.
    const collision = source.replace('let callback', 'let identity fn(Int) Int = fn(value Int) Int {\n        value + 1\n    }\n    let callback');
    await client.change(app, collision, 2);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', {
      ...at(app, collision, 'answer\n'), newName: 'identity',
    }), /conflict/);
    await client.change(app, source, 3);

    // An invalid unopened test blocks the edit instead of being silently omitted.
    await fs.writeFile(path.join(folder, 'library/main_test.loom'), 'test fn broken() {\n    missing()\n}\n');
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...imported, newName: 'identity' }), /Cannot check module package/);
    await fs.writeFile(path.join(folder, 'library/main_test.loom'), sources.get('library/main_test.loom'));

    const external = `import std.int.minimum\n${source}`;
    await client.change(app, external, 4);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', {
      ...at(app, external, 'minimum\n'), newName: 'smallest',
    }), /selected module/);
    await client.change(app, source, 5);

    for (const [name, text] of sources) assert.equal(await fs.readFile(path.join(folder, name), 'utf8'), text);
    assert.equal(await client.rpc.sendRequest('textDocument/rename', { ...params, newName: 'identity' }), null);
    console.log('Public function rename passed: module-wide imports, callbacks, independent tests, overlays, conflicts and no source writes.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}
module.exports = { publicRenameSmoke };

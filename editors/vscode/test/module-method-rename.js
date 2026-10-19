'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');

async function moduleMethodRenameSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-module-method-'));
  const folder = await fs.realpath(temporary);
  const sources = new Map([
    ['loom.toml', "[module]\nname = 'demo'\n"],
    ['library/main.loom', `pub concept Read {
    fn read(self Self, value Int) Int
    fn read(self Self, value Bool) Bool
}

impl Read for Int {
    fn read(self Int, value Int) Int {
        value
    }
    fn read(self Int, value Bool) Bool {
        value
    }
}

pub fn forward[T Read](value T) Int {
    value.read(3)
}

test fn embedded() {
    assert Read.read(1, 4) == 4
}
`],
    ['library/main_test.loom', `test fn own_test() {
    let erased dyn Read = 5
    assert erased.read(2) == 2
    assert 1.read(true)
}
`],
    ['app/main.loom', `import demo.library.Read
import demo.library.forward

impl Read for Bool {
    fn read(self Bool, value Int) Int {
        value
    }
    fn read(self Bool, value Bool) Bool {
        value
    }
}

fn main() {
    let action fn(Int) Int = true.read
    assert action(7) == 7
    assert true.read(4) == 4
    assert Read.read(true, 5) == 5
    assert forward(true) == 3
    assert true.read(false) == false
}
`],
    ['app/main_test.loom', `test fn own_test() {
    assert true.read(9) == 9
}
`],
    ['adapter/main.loom', `import demo.library.Read

impl[Ts...] Read for (Ts...) {
    fn read(self Self, value Int) Int {
        value
    }
    fn read(self Self, value Bool) Bool {
        value
    }
}
`],
    ['adapter/main_test.loom', `test fn own_test() {
    assert (1, false).read(8) == 8
    assert (1, false).read(true)
}
`],
    ['other/main.loom', `concept Other {
    fn read(self Self, value Int) Int
}

impl Other for Bool {
    fn read(self Bool, value Int) Int {
        value * 2
    }
}

test fn own_test() {
    assert true.read(7) == 14
}
`],
  ]);
  for (const [relative, text] of sources) {
    const file = path.join(folder, relative);
    await fs.mkdir(path.dirname(file), { recursive: true });
    await fs.writeFile(file, text);
  }
  const app = path.join(folder, 'app/main.loom');
  const library = path.join(folder, 'library/main.loom');
  const source = sources.get('app/main.loom');
  const client = await session({ executable, stdRoot }, folder);
  const at = (relative, fragment) => {
    const file = path.join(folder, relative), text = sources.get(relative);
    const uri = URI.file(file).toString();
    const offset = text.indexOf(fragment);
    assert.notEqual(offset, -1, fragment);
    return { textDocument: { uri }, position: TextDocument.create(uri, 'loom', 1, text).positionAt(offset) };
  };
  try {
    await client.open(app, source);
    assert.deepEqual((await client.wait(app, 1)).diagnostics, []);
    const edit = await client.rpc.sendRequest('textDocument/rename', {
      ...at('app/main.loom', 'read(4)'), newName: 'fetch',
    });
    const expected = [...sources.keys()].filter(file => file.endsWith('.loom') && !file.startsWith('other/'));
    assert.deepEqual(Object.keys(edit.changes).sort(), expected.map(file => URI.file(path.join(folder, file)).toString()).sort());
    const revised = new Map();
    for (const relative of expected) {
      const file = path.join(folder, relative), uri = URI.file(file).toString();
      const document = TextDocument.create(uri, 'loom', 1, sources.get(relative));
      assert.ok(edit.changes[uri].every(change => document.getText(change.range) === 'read'));
      revised.set(relative, TextDocument.applyEdits(document, edit.changes[uri]));
    }
    assert.ok(revised.get('app/main.loom').includes('import demo.library.Read'));
    assert.ok(revised.get('app/main.loom').includes('true.read(false)'));
    assert.ok(revised.get('library/main_test.loom').includes('1.read(true)'));
    assert.ok(revised.get('adapter/main_test.loom').includes('.read(true)'));
    const locations = await client.rpc.sendRequest('textDocument/references', {
      ...at('app/main.loom', 'read(4)'), context: { includeDeclaration: true },
    });
    const count = Object.values(edit.changes).reduce((sum, changes) => sum + changes.length, 0);
    assert.equal(locations.length, count);
    await client.open(library, sources.get('library/main.loom'));
    assert.deepEqual((await client.wait(library, 1)).diagnostics, []);
    assert.deepEqual(await client.rpc.sendRequest('textDocument/rename', {
      ...at('library/main.loom', 'read(self Self, value Int)'), newName: 'fetch',
    }), edit);

    const unknown = path.join(folder, 'unknown/main.loom');
    await fs.mkdir(path.dirname(unknown));
    await client.open(unknown, `fn probe() Int {
    comptime if true {
        0
    } else {
        true.read(99)
    }
}
`);
    assert.deepEqual((await client.wait(unknown, 1)).diagnostics, []);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', {
      ...at('app/main.loom', 'read(4)'), newName: 'fetch',
    }), /every occurrence/);
    const partial = await client.rpc.sendRequest('textDocument/references', {
      ...at('app/main.loom', 'read(4)'), context: { includeDeclaration: true },
    });
    assert.equal(partial.length, count);
    await client.rpc.sendNotification('textDocument/didClose', {
      textDocument: { uri: URI.file(unknown).toString() },
    });
    assert.deepEqual(await client.rpc.sendRequest('textDocument/rename', {
      ...at('app/main.loom', 'read(4)'), newName: 'fetch',
    }), edit);
    await assert.rejects(fs.access(unknown), { code: 'ENOENT' });

    // Recheck the complete virtual module without saving any of the edits.
    for (const [relative, text] of revised) {
      const file = path.join(folder, relative);
      if (file === app || file === library) await client.change(file, text, 2);
      else await client.open(file, text);
    }
    await client.change(app, revised.get('app/main.loom'), 3);
    assert.deepEqual((await client.wait(app, 3)).diagnostics, []);
    for (const [relative, text] of sources) {
      assert.equal(await fs.readFile(path.join(folder, relative), 'utf8'), text);
    }
    console.log('Module method rename passed: one public slot, provider/consumer/independent impl packages, private tests, qualified/bound/dyn/generic uses and unrelated methods.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

module.exports = { moduleMethodRenameSmoke };

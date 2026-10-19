'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');

async function methodReferencesSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-method-references-'));
  const folder = await fs.realpath(temporary);
  const file = path.join(folder, 'main.loom');
  const source = `concept Read {
    fn read(self Self, value Int) Int
    fn read(self Self, value Bool) Bool
}

concept Other {
    fn read(self Self) Text
}

impl Read for Bool {
    fn read(self Bool, value Int) Int {
        value
    }
    fn read(self Bool, value Bool) Bool {
        value
    }
}

impl Read for Int {
    fn read(self Int, value Int) Int {
        value
    }
    fn read(self Int, value Bool) Bool {
        value
    }
}

impl Other for Bool {
    fn read(self Bool) Text {
        "other"
    }
}

fn forwarded[S Read](source S) Int {
    source.read(11)
}

fn main() {
    let number = 2
    let erased dyn Read = true
    let action fn(Int) Int = true.read
    assert true.read(7) == 7
    assert number.read(8) == 8
    assert erased.read(9) == 9
    assert action(10) == 10
    assert forwarded(true) == 11
    assert true.read(false) == false
    assert true.read() == "other"
}
`;
  await fs.writeFile(file, source);
  const client = await session({ executable, stdRoot }, folder);
  try {
    await client.open(file, source);
    assert.deepEqual((await client.wait(file, 1)).diagnostics, []);
    const uri = URI.file(file).toString();
    const document = TextDocument.create(uri, 'loom', 1, source);
    const offset = (fragment, prefix = '') => {
      const found = source.indexOf(fragment);
      assert.notEqual(found, -1, fragment);
      return found + prefix.length;
    };
    const integer = [
      offset('read(self Self, value Int)'), offset('read(self Bool, value Int)'),
      offset('read(self Int, value Int)'), offset('source.read(11)', 'source.'),
      offset('true.read\n', 'true.'), offset('true.read(7)', 'true.'),
      offset('number.read(8)', 'number.'), offset('erased.read(9)', 'erased.'),
    ].sort((a, b) => a - b);
    const references = async (position, includeDeclaration) => {
      const result = await client.rpc.sendRequest('textDocument/references', {
        textDocument: { uri }, position: document.positionAt(position),
        context: { includeDeclaration },
      });
      for (const location of result) {
        assert.equal(location.uri, uri);
        assert.equal(document.getText(location.range), 'read');
      }
      return result.map(location => document.offsetAt(location.range.start)).sort((a, b) => a - b);
    };
    for (const selected of integer) {
      assert.deepEqual(await references(selected, true), integer);
      assert.deepEqual(await references(selected, false), integer.slice(1));
    }
    const boolean = [
      offset('read(self Self, value Bool)'), offset('read(self Bool, value Bool)'),
      offset('read(self Int, value Bool)'), offset('true.read(false)', 'true.'),
    ].sort((a, b) => a - b);
    assert.deepEqual(await references(boolean[0], true), boolean);
    assert.equal(await fs.readFile(file, 'utf8'), source);
    console.log('Method references passed: checked overload families, impls, generic/dyn calls, bound values and declaration filtering.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

module.exports = { methodReferencesSmoke };

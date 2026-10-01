'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');

async function typeRenameSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-type-rename-'));
  const folder = await fs.realpath(temporary);
  const sources = new Map([
    ['loom.toml', "[module]\nname = 'demo'\n"],
    ['library/main.loom', `pub record Token {
    value Int
}
pub record Envelope {
    item Token
    history List[Token]
}
pub enum Choice {
    Some(Token)
    Empty
}
pub type Positive = Int where self > 0
pub fn echo(value Token) Token {
    value
}
test fn embedded() {
    assert Token { value = 4 }.value == 4
}
`],
    ['library/main_test.loom', `test fn library_test() {
    assert Token { value = 5 }.value == 5
    assert Positive(7) == 7
}
`],
    ['app/main.loom', `// é😀 Token stays in comments and strings.
import demo.library.Token
import demo.library.Choice
import demo.library.Positive
import demo.library.echo

fn copy(value Token) Token {
    value
}
fn main() {
    let value Token = Token { value = 7 }
    assert copy(echo(value)).value == 7
    let choice Choice = Choice.Some(value)
    assert match choice {
        Choice.Some(item) => item.value == 7
        Choice.Empty => false
    }
    let positive Positive = Positive(7)
    assert positive == 7
    assert "Token" == "Token"
}
`],
    ['app/main_test.loom', `test fn app_test() {
    let value Token = Token { value = 8 }
    assert echo(value).value == 8
    discard Choice.Empty
}
`],
    ['other/main.loom', `pub record Token {
    other Bool
}
`],
    ['private/main.loom', `record Hidden {
    count Int
}
fn main() {
    let value Hidden = Hidden { count = 7 }
    assert value.count == 7
}
`],
    ['private/main_test.loom', `test fn private_test() {
    assert Hidden { count = 8 }.count == 8
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
  const at = (file, text, fragment) => ({
    textDocument: { uri: URI.file(file).toString() },
    position: TextDocument.create(URI.file(file).toString(), 'loom', 1, text).positionAt(text.indexOf(fragment)),
  });
  const token = at(app, source, 'Token) Token');
  try {
    await client.open(app, source);
    await client.open(library, original);
    assert.deepEqual((await client.wait(app, 1)).diagnostics, []);
    for (const [name, replacement, count] of [['Token', 'Entry', 15], ['Choice', 'Selection', 7], ['Positive', 'Nonzero', 5]]) {
      const selected = name === 'Token' ? token : at(app, source, `${name} =`);
      const definitions = await client.rpc.sendRequest('textDocument/definition', selected);
      assert.equal(definitions.length, 1, JSON.stringify(definitions));
      assert.equal(definitions[0].uri, URI.file(library).toString());
      assert.deepEqual(await client.rpc.sendRequest('textDocument/definition', at(app, source, `${name}\n`)), definitions);
      const references = await client.rpc.sendRequest('textDocument/references', { ...selected, context: { includeDeclaration: true } });
      assert.equal(references.length, count, `${name}: ${JSON.stringify(references)}`);
      const edit = await client.rpc.sendRequest('textDocument/rename', { ...selected, newName: replacement });
      assert.equal(Object.values(edit.changes).flat().length, count, JSON.stringify(edit));
      assert.ok(!edit.changes[URI.file(path.join(folder, 'other/main.loom')).toString()]);
      for (const [uri, edits] of Object.entries(edit.changes)) {
        const text = sources.get(path.relative(folder, URI.parse(uri).fsPath).split(path.sep).join('/'));
        const changed = TextDocument.applyEdits(TextDocument.create(uri, 'loom', 1, text), edits);
        if (uri === URI.file(app).toString()) {
          assert.ok(changed.includes('// é😀 Token') && changed.includes('"Token"'));
        }
      }
      const fromDeclaration = await client.rpc.sendRequest('textDocument/rename', {
        ...at(library, original, `${name} `), newName: replacement,
      });
      assert.deepEqual(fromDeclaration, edit);
    }
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...token, newName: 'Envelope' }), /conflict/);

    // A specialization does not turn a same-spelled type parameter into Token.
    await client.change(library, `${original}\nfn identity[Token](value Token) Token {\n    value\n}\n`, 2);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...token, newName: 'Entry' }), /every occurrence/);
    await client.change(library, original, 3);

    const shadowed = `${original}\nfn typed(comptime Token type, value Token) Token {\n    value\n}\ntest fn alias_test() {\n    assert typed(Token, Token { value = 7 }).value == 7\n}\n`;
    await client.change(library, shadowed, 4);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...token, newName: 'Entry' }), /every occurrence/);
    await client.change(library, original, 5);

    const privateFile = path.join(folder, 'private/main.loom'), privateSource = sources.get('private/main.loom');
    await client.open(privateFile, privateSource);
    const privateEdit = await client.rpc.sendRequest('textDocument/rename', {
      ...at(privateFile, privateSource, 'Hidden {'), newName: 'Internal',
    });
    assert.equal(Object.values(privateEdit.changes).flat().length, 4);
    assert.deepEqual(Object.keys(privateEdit.changes).sort(), ['private/main.loom', 'private/main_test.loom']
      .map(name => URI.file(path.join(folder, name)).toString()).sort());

    for (const [name, text] of sources) assert.equal(await fs.readFile(path.join(folder, name), 'utf8'), text);
    console.log('Nominal type rename passed: checked headers, fields, variants, local annotations, imports, tests and shadow refusal.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}
module.exports = { typeRenameSmoke };

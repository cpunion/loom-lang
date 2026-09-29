'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const { URI } = require('vscode-uri');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { session } = require('./session');

async function bindingRenameSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-binding-rename-'));
  const folder = await fs.realpath(temporary), file = path.join(folder, 'main.loom');
  const uri = URI.file(file).toString();
  const client = await session({ executable, stdRoot }, folder);
  const source = `import std.option.Option

record Pair {
    left Int
    right Int
}

concept Add {
    fn add(self Self, extra Int) Int
}

impl Add for Pair {
    fn add(self Self, extra Int) Int {
        self.left + extra
    }
}

fn identity(amount Int) Int
requires amount >= 0
ensures result == amount
{
    amount
}

fn choose(value Option[Int]) Int {
    match value {
        Option.Some(item) if item > 0 => item
        Option.Some(item) => -item
        Option.None => 0
    }
}

fn unguarded(value Option[Int]) Int {
    match value {
        Option.Some(payload) => payload
        Option.None => 0
    }
}

fn whole(value Option[Int]) Int {
    match value {
        copy => unguarded(copy)
    }
}

fn main() {
    let (first, second) = (1, 2)
    let Pair { left = chosen, right = _ } = Pair { left = first right = second }
    assert identity(chosen) == 1
    assert choose(Option.Some(first)) == 1
    assert whole(Option.Some(second)) == 2
    assert Pair { left = first right = second }.add(3) == 4
}
`;
  let version = 1;
  try {
    await client.open(file, source);
    assert.deepEqual((await client.wait(file, version)).diagnostics, []);
    // Definition, references and rename must agree from both ends. Contracts,
    // disjoint match arms and field labels are part of the same source trial.
    for (const [fragment, name, count] of [
      ['amount Int', 'amount', 4],
      ['amount >=', 'amount', 4],
      ['first right', 'first', 4],
      ['chosen)', 'chosen', 2],
      ['item >', 'item', 3],
      ['-item', 'item', 2],
      ['payload\n', 'payload', 2],
      ['copy)', 'copy', 2],
      ['extra\n', 'extra', 2],
    ]) {
      const document = TextDocument.create(uri, 'loom', version, source);
      const start = source.indexOf(fragment) + (fragment.startsWith('-') ? 1 : 0);
      assert.ok(start >= 0);
      const params = { textDocument: { uri }, position: document.positionAt(start) };
      const references = await client.rpc.sendRequest('textDocument/references', {
        ...params, context: { includeDeclaration: true },
      });
      assert.equal(references.length, count, fragment);
      const edits = await client.rpc.sendRequest('textDocument/rename', { ...params, newName: 'renamed' });
      assert.equal(edits.changes[uri].length, count, fragment);
      assert.ok(edits.changes[uri].every(edit => document.getText(edit.range) === name));
      const revised = TextDocument.applyEdits(document, edits.changes[uri]);
      assert.ok(revised.includes('left Int') && revised.includes(`left = ${name === 'chosen' ? 'renamed' : 'chosen'}`));
      await client.change(file, revised, ++version);
      assert.deepEqual((await client.wait(file, version)).diagnostics, [], fragment);
      await client.change(file, source, ++version);
      assert.deepEqual((await client.wait(file, version)).diagnostics, []);
    }
    const document = TextDocument.create(uri, 'loom', version, source);
    const params = { textDocument: { uri }, position: document.positionAt(source.indexOf('amount >=')) };
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...params, newName: 'identity' }), /conflict/);
    await assert.rejects(fs.readFile(file), { code: 'ENOENT' });
    console.log('Binding rename smoke passed: parameters/contracts, destructuring, guarded/whole match bindings and method locals.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

module.exports = { bindingRenameSmoke };

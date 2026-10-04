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
import std.resource.Dispose
import std.resource.MustScope

record Pair {
    left Int
    right Int
}

record Guard {
    count Int
}

impl MustScope for Guard {
}

impl Dispose for Guard {
    fn dispose(self Guard) {
    }
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

fn unused(ignored Int) Int {
    7
}

fn static_added(comptime width Int, value Int) Int {
    let first = width + value
    {
        let width = 3
        discard width
    }
    first
}

fn static_computed(comptime count Int) Int {
    comptime { count + 1 }
}

fn static_type(comptime selected type) Bool {
    comptime if selected == Int {
        comptime if List[selected] == List[Int] {
            true
        } else {
            false
        }
    } else {
        false
    }
}

fn static_generic[T](comptime sample T) T {
    sample
}

fn static_unused(comptime unused_static Int) Int {
    1
}

fn static_applied(comptime action fn(Int) Int, value Int) Int {
    action(value)
}

fn static_selected(comptime flag Bool, comptime amount Int) Int {
    comptime if flag {
        amount
    } else {
        amount + 1
    }
}

fn increment(value Int) Int {
    value + 1
}

fn static_partial(comptime chosen Bool, comptime missing Int) Int {
    comptime if chosen {
        missing
    } else {
        missing + 2
    }
}

fn runtime_selected(comptime chosen Bool, input Int) Int {
    comptime if chosen {
        let computed = input + 1
        computed
    } else {
        input + 2
    }
}

fn runtime_partial(comptime chosen Bool, pending Int) Int {
    comptime if chosen {
        pending
    } else {
        pending + 2
    }
}

fn main() {
    scoped guard = Guard { count = 1 }
    assert guard.count == 1
    let (first, second) = (1, 2)
    let Pair { left = chosen, right = _ } = Pair { left = first right = second }
    assert identity(chosen) == 1
    assert choose(Option.Some(first)) == 1
    assert whole(Option.Some(second)) == 2
    assert Pair { left = first right = second }.add(3) == 4
    assert unused(3) == 7
    assert static_added(1, 2) == 3
    assert static_computed(4) == 5
    assert static_type(Int) && !static_type(Bool)
    assert static_generic(7) == 7
    assert static_generic(true)
    assert static_unused(9) == 1
    assert static_applied(increment, 3) == 4
    assert static_selected(true, 5) == 5
    assert static_selected(false, 5) == 6
    assert static_partial(true, 1) == 1
    assert runtime_selected(true, 1) == 2
    assert runtime_selected(false, 1) == 3
    assert runtime_partial(true, 1) == 1
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
      ['guard.count', 'guard', 2],
      ['ignored Int', 'ignored', 1],
      ['width + value', 'width', 2],
      ['count + 1', 'count', 2],
      ['action(value)', 'action', 2],
      ['flag Bool', 'flag', 2],
      ['amount + 1', 'amount', 3],
      ['value Int) Int {\n    let first', 'value', 2],
      ['input Int', 'input', 3],
      ['computed = input', 'computed', 2],
      ['sample T', 'sample', 2],
      ['selected == Int', 'selected', 3],
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
    for (const [fragment, declaration, labels] of [
      ['width + value', 'width Int', ['Int']],
      ['count + 1', 'count Int', ['Int']],
      ['action(value)', 'action fn', ['fn(Int) Int']],
      ['flag Bool', 'flag Bool', ['Bool']],
      ['sample\n', 'sample T', ['Int', 'Bool']],
      ['unused_static Int', 'unused_static Int', ['Int']],
      ['selected == Int', 'selected type', ['type']],
    ]) {
      const params = { textDocument: { uri }, position: document.positionAt(source.indexOf(fragment)) };
      const definitions = await client.rpc.sendRequest('textDocument/definition', params);
      assert.equal(definitions.length, 1, fragment);
      assert.equal(definitions[0].uri, uri, fragment);
      assert.deepEqual(definitions[0].range.start,
        document.positionAt(source.indexOf(`comptime ${declaration}`) + 'comptime '.length), fragment);
      const hover = await client.rpc.sendRequest('textDocument/hover', params);
      assert.deepEqual(hover.contents.map(item => item.value).sort(), [...labels].sort(), fragment);
    }
    const params = { textDocument: { uri }, position: document.positionAt(source.indexOf('amount >=')) };
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', { ...params, newName: 'identity' }), /conflict/);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', {
      textDocument: { uri }, position: document.positionAt(source.indexOf('missing Int')), newName: 'renamed',
    }), /every occurrence of this compile-time parameter/);
    await assert.rejects(client.rpc.sendRequest('textDocument/rename', {
      textDocument: { uri }, position: document.positionAt(source.indexOf('pending Int')), newName: 'renamed',
    }), /every occurrence of this local binding/);
    assert.equal(await client.rpc.sendRequest('textDocument/rename', {
      textDocument: { uri }, position: document.positionAt(source.indexOf('Option.None') + 7), newName: 'Changed',
    }), null);
    const fieldEdit = await client.rpc.sendRequest('textDocument/rename', {
      textDocument: { uri }, position: document.positionAt(source.indexOf('left = chosen')), newName: 'changed',
    });
    assert.equal(fieldEdit.changes[uri].length, 5);
    assert.ok(fieldEdit.changes[uri].every(edit => document.getText(edit.range) === 'left'));
    const fieldSource = TextDocument.applyEdits(document, fieldEdit.changes[uri]);
    assert.ok(fieldSource.includes('changed Int') && fieldSource.includes('self.changed + extra'));
    await client.change(file, fieldSource, ++version);
    assert.deepEqual((await client.wait(file, version)).diagnostics, []);
    await client.change(file, source, ++version);
    await assert.rejects(fs.readFile(file), { code: 'ENOENT' });
    console.log('Binding rename smoke passed: runtime/static parameters, specialization coverage, contracts, patterns and method locals.');
  } finally {
    await client.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

module.exports = { bindingRenameSmoke };

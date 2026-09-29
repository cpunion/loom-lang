'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const compiler = require('../compiler');

async function residentSmoke(executable, stdRoot) {
  const temporary = await fs.mkdtemp(path.join(os.tmpdir(), 'loom-resident-'));
  const folder = await fs.realpath(temporary);
  const file = path.join(folder, 'main.loom'), input = path.join(folder, 'gate.txt');
  const settings = { executable, stdRoot };
  const source = `import std.build.input_file
import std.build.option

fn chosen() Int {
    7
}

fn main() {
    let value = chosen()
    comptime if input_file("gate.txt") == "ready" && option("app.mode", "on") == "on" {
    } else {
        missing()
    }
    assert value == 7
}
`;
  const offset = Buffer.byteLength(source.slice(0, source.indexOf('value ==')));
  try {
    await fs.writeFile(path.join(folder, 'loom.toml'), '[module]\nname = "resident"\n');
    await fs.writeFile(file, source);
    await fs.writeFile(input, 'ready');
    const cold = await compiler.check(settings, folder, []);
    assert.deepEqual(cold.diagnostics, []);
    assert.equal(cold.analysisReused, false);
    const warm = await compiler.check(settings, folder, []);
    assert.equal(warm.analysisReused, true);
    assert.deepEqual(warm.buildInputs, cold.buildInputs);
    const query = await compiler.query(settings, folder, file, offset, []);
    assert.equal(query.analysisReused, true);
    assert.ok(query.hover.types.includes('Int'));
    const references = await compiler.symbols(settings, folder, file,
      Buffer.byteLength(source.slice(0, source.lastIndexOf('chosen()'))), []);
    assert.equal(references.analysisReused, true);
    assert.equal(references.references.length, 2);

    // No client notification: the compiler itself must detect changed input bytes.
    await fs.writeFile(input, 'wrong');
    const changed = await compiler.check(settings, folder, []);
    assert.equal(changed.analysisReused, false);
    assert.ok(changed.diagnostics.length);
    await fs.writeFile(input, 'ready');
    assert.equal((await compiler.check(settings, folder, [])).analysisReused, true);
    const configured = await compiler.check({ ...settings, buildOptions: { 'app.mode': 'off' } }, folder, []);
    assert.equal(configured.analysisReused, false);
    assert.ok(configured.diagnostics.length);

    const sibling = path.join(folder, 'added.loom');
    await fs.writeFile(sibling, 'fn chosen() Bool {\n    true\n}\n');
    assert.ok((await compiler.check(settings, folder, [])).diagnostics.length);
    await fs.unlink(sibling);
    const overlay = await compiler.snapshots([{ path: file,
      text: source.replace('chosen() Int', 'chosen() Bool').replace('    7\n', '    true\n').replace('value == 7', 'value') }]);
    try {
      assert.equal((await compiler.check(settings, folder, overlay.args)).analysisReused, false);
      const hover = await compiler.query(settings, folder, file, offset + 4, overlay.args);
      assert.equal(hover.analysisReused, true);
      assert.ok(hover.hover.types.includes('Bool'));
    } finally { await overlay.dispose(); }
    assert.equal((await compiler.check(settings, folder, [])).analysisReused, false);
    assert.equal(await fs.readFile(file, 'utf8'), source);

    const ordinary = `fn leaf(value Int) Int {
    value + 1
}

fn middle(value Int) Int {
    leaf(value)
}

fn independent(value Int) Int {
    value * 2
}

fn main() {
    let value = middle(3)
    assert value > 0
}
`;
    await fs.writeFile(file, ordinary);
    assert.deepEqual((await compiler.check(settings, folder, [])).diagnostics, []);
    const edited = ordinary.replace('value + 1', '\n    value + 12');
    await fs.writeFile(file, edited);
    const incremental = await compiler.check(settings, folder, []);
    assert.deepEqual(incremental.diagnostics, []);
    assert.equal(incremental.analysisReused, false);
    assert.equal(incremental.definitionsReused, 1);
    const location = Buffer.byteLength(edited.slice(0, edited.lastIndexOf('middle(3)')));
    const updated = await compiler.symbols(settings, folder, file, location, []);
    assert.equal(updated.references.length, 2);
    assert.ok(updated.references.some(reference => reference.start === edited.indexOf('middle(value')));
    await fs.writeFile(file, edited.replace('value + 12', 'true'));
    assert.ok((await compiler.check(settings, folder, [])).diagnostics.length);

    const generated = `import std.build.input_file
comptime {
    input_file("gate.txt")
}
fn main() {
    assert generated() == 42
}
`;
    await fs.writeFile(file, generated);
    await fs.writeFile(input, 'fn generated() Int {\n    42\n}\n');
    assert.deepEqual((await compiler.check(settings, folder, [])).diagnostics, []);
    const generatedOffset = generated.lastIndexOf('generated()');
    const generatedHover = await compiler.query(settings, folder, file, generatedOffset, []);
    assert.equal(generatedHover.analysisReused, true);
    assert.ok(generatedHover.hover.types.includes('fn() Int'));
    assert.equal(generatedHover.definitions[0].start, generated.indexOf('comptime'));
    const generatedRename = await compiler.symbols(settings, folder, file, generatedOffset, [], undefined, 'renamed');
    assert.ok(!generatedRename.edits?.length);
    await fs.writeFile(input, 'fn generated() Bool {\n    true\n}\n');
    const regenerated = await compiler.check(settings, folder, []);
    assert.equal(regenerated.analysisReused, false);
    assert.equal(regenerated.definitionsReused, 0);
    assert.ok(regenerated.diagnostics.length);
    console.log('Resident editor smoke passed: snapshot/definition reuse, dependency invalidation, current source spans, unsaved type changes.');
  } finally {
    compiler.close();
    await fs.rm(temporary, { recursive: true, force: true });
  }
}

module.exports = { residentSmoke };
if (require.main === module) {
  residentSmoke(path.resolve(process.env.LOOM_COMPILER || '../../target/loom'), path.resolve('../../compiler/std'))
    .catch(error => { console.error(error); process.exitCode = 1; });
}

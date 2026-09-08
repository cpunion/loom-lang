'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const vscode = require('vscode');

function diagnostics(uri, predicate) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => { listener.dispose(); reject(new Error('Timed out waiting for Loom diagnostics')); }, 20000);
    function changed() {
      const values = vscode.languages.getDiagnostics(uri);
      if (predicate(values)) { clearTimeout(timer); listener.dispose(); resolve(values); }
    }
    const listener = vscode.languages.onDidChangeDiagnostics(changed);
    changed();
  });
}

exports.run = async function run() {
  const settings = vscode.workspace.getConfiguration('loom');
  await settings.update('executable', process.env.LOOM_EDITOR_COMPILER, vscode.ConfigurationTarget.Global);
  await settings.update('stdRoot', process.env.LOOM_EDITOR_STD, vscode.ConfigurationTarget.Global);
  const extension = vscode.extensions.getExtension('cpunion.loom-language');
  assert.ok(extension);
  await extension.activate();
  const file = path.join(__dirname, 'fixtures/project/main.loom');
  const original = await fs.readFile(file, 'utf8');
  const document = await vscode.workspace.openTextDocument(file);
  assert.equal(document.languageId, 'loom');
  const editor = await vscode.window.showTextDocument(document);
  const replace = text => editor.edit(edit => edit.replace(new vscode.Range(document.positionAt(0), document.positionAt(document.getText().length)), text));
  try {
    await replace('fn main() { let 值 Int = true\ndiscard 值 }\n');
    const errors = await diagnostics(document.uri, values => values.some(value => value.source === 'loom'));
    assert.equal(errors[0].severity, vscode.DiagnosticSeverity.Error);
    await replace('fn amount() Int { 42 }\nfn main(){discard "é😀"\nlet value=amount()\nassert value==42}\n');
    await diagnostics(document.uri, values => values.length === 0);
    const hovers = await vscode.commands.executeCommand('vscode.executeHoverProvider', document.uri, new vscode.Position(3, 7));
    assert.ok(hovers.some(hover => hover.contents.some(content => /\bInt\b/.test(content.value))));
    const definitions = await vscode.commands.executeCommand('vscode.executeDefinitionProvider', document.uri, new vscode.Position(2, 11));
    assert.equal(definitions.length, 1);
    assert.equal((definitions[0].uri || definitions[0].targetUri).toString(), document.uri.toString());
    assert.match(document.getText(definitions[0].range || definitions[0].targetSelectionRange), /amount/);
    const edits = await vscode.commands.executeCommand('vscode.executeFormatDocumentProvider', document.uri, { tabSize: 4, insertSpaces: true });
    assert.ok(edits.length > 0);
    const change = new vscode.WorkspaceEdit();
    change.set(document.uri, edits);
    assert.ok(await vscode.workspace.applyEdit(change));
    assert.match(document.getText(), /fn main\(\) \{/);
    assert.equal(await fs.readFile(file, 'utf8'), original);
  } finally { await vscode.commands.executeCommand('workbench.action.revertAndCloseActiveEditor'); }
};

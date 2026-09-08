'use strict';
const vscode = require('vscode');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');
let client;

async function activate(context) {
  async function start() {
    if (client || !vscode.workspace.isTrusted) return;
    const watcher = vscode.workspace.createFileSystemWatcher('**/*.{loom,toml,lock}');
    context.subscriptions.push(watcher);
    client = new LanguageClient('loom', 'Loom', {
      module: context.asAbsolutePath('server.js'), transport: TransportKind.ipc,
    }, {
      documentSelector: [{ scheme: 'file', language: 'loom' }],
      synchronize: { configurationSection: 'loom', fileEvents: watcher },
    });
    await client.start();
  }
  context.subscriptions.push(vscode.workspace.onDidGrantWorkspaceTrust(start));
  await start();
}

async function deactivate() { await client?.dispose(); client = undefined; }
module.exports = { activate, deactivate };

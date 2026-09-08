'use strict';
const { spawn } = require('node:child_process');
const path = require('node:path');
const { createProtocolConnection, StreamMessageReader, StreamMessageWriter } = require('vscode-languageserver-protocol/node');
const { URI } = require('vscode-uri');

async function session(settings, folder, configuration) {
  const child = spawn(process.execPath, [path.resolve(__dirname, '../server.js'), '--stdio'], { stdio: ['pipe', 'pipe', 'pipe'] });
  let stderr = '';
  child.stderr.on('data', data => { stderr += data; });
  const rpc = createProtocolConnection(new StreamMessageReader(child.stdout), new StreamMessageWriter(child.stdin));
  const diagnostics = [], waiters = [];
  rpc.onNotification('textDocument/publishDiagnostics', report => {
    diagnostics.push(report);
    for (const check of [...waiters]) check();
  });
  rpc.onNotification('window/showMessage', message => { stderr += message.message; });
  if (configuration) rpc.onRequest('workspace/configuration', params => params.items.map(item => configuration(item.scopeUri)));
  rpc.listen();
  const initialized = await rpc.sendRequest('initialize', { processId: process.pid,
    capabilities: configuration ? { workspace: { configuration: true } } : {},
    workspaceFolders: (Array.isArray(folder) ? folder : [folder]).map(folder => ({ uri: URI.file(folder).toString(), name: path.basename(folder) })),
    initializationOptions: { settings } });
  await rpc.sendNotification('initialized', {});
  return {
    rpc, diagnostics, initialized,
    open(file, text, version = 1) {
      return rpc.sendNotification('textDocument/didOpen', { textDocument: { uri: URI.file(file).toString(), languageId: 'loom', version, text } });
    },
    change(file, text, version) {
      return rpc.sendNotification('textDocument/didChange', { textDocument: { uri: URI.file(file).toString(), version }, contentChanges: [{ text }] });
    },
    wait(file, version) {
      const uri = URI.file(file).toString();
      return new Promise((resolve, reject) => {
        const timeout = setTimeout(() => { remove(); reject(new Error(`No diagnostics for version ${version}: ${stderr}`)); }, 15000);
        function remove() { clearTimeout(timeout); const index = waiters.indexOf(check); if (index >= 0) waiters.splice(index, 1); }
        function check() {
          const report = diagnostics.find(report => report.uri === uri && report.version === version);
          if (report) { remove(); resolve(report); }
        }
        waiters.push(check); check();
      });
    },
    async close() {
      await rpc.sendRequest('shutdown');
      await rpc.sendNotification('exit');
      rpc.dispose();
      if (child.exitCode === null) await new Promise(resolve => child.once('exit', resolve));
    },
  };
}
module.exports = { session };

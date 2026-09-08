'use strict';
const { createConnection, TextDocuments, TextDocumentSyncKind, DiagnosticSeverity, ResponseError, LSPErrorCodes } = require('vscode-languageserver/node');
const { TextDocument } = require('vscode-languageserver-textdocument');
const { URI } = require('vscode-uri');
const fs = require('node:fs/promises');
const path = require('node:path');
const compiler = require('./compiler');

const connection = createConnection();
const documents = new TextDocuments(TextDocument);
let folders = [], configuration = false, folderChanges = false, defaults = {}, generation = 0, timer, checking, lastError;
const published = new Set();
const pending = new Set();

connection.onInitialize(params => {
  folders = params.workspaceFolders?.map(folder => URI.parse(folder.uri).fsPath) || (params.rootUri ? [URI.parse(params.rootUri).fsPath] : []);
  configuration = !!params.capabilities.workspace?.configuration;
  folderChanges = !!params.capabilities.workspace?.workspaceFolders;
  defaults = params.initializationOptions?.settings || {};
  return { capabilities: { textDocumentSync: TextDocumentSyncKind.Incremental, documentFormattingProvider: true,
    workspace: { workspaceFolders: { supported: true, changeNotifications: true } } } };
});
connection.onInitialized(() => {
  if (folderChanges) connection.workspace.onDidChangeWorkspaceFolders(event => {
    const removed = event.removed.map(folder => URI.parse(folder.uri).fsPath);
    folders = [...folders.filter(folder => !removed.includes(folder)), ...event.added.map(folder => URI.parse(folder.uri).fsPath)];
    schedule();
  });
});

async function settings(uri) {
  const raw = configuration ? await connection.workspace.getConfiguration({ scopeUri: uri, section: 'loom' }) : defaults;
  const file = URI.parse(uri).fsPath;
  const folder = folders.filter(root => file === root || file.startsWith(root + path.sep)).sort((a, b) => b.length - a.length)[0] || path.dirname(file);
  let executable = raw?.executable || 'loom';
  if (!path.isAbsolute(executable) && /[/\\]/.test(executable)) executable = path.resolve(folder, executable);
  return { executable, stdRoot: raw?.stdRoot ? path.resolve(folder, raw.stdRoot) : '' };
}

function schedule() {
  generation++;
  clearTimeout(timer);
  checking?.abort();
  timer = setTimeout(() => {
    const task = validate(generation);
    pending.add(task);
    task.finally(() => pending.delete(task));
  }, 350);
}

function diagnostic(document, item) {
  return { message: item.message, severity: DiagnosticSeverity.Error, source: 'loom',
    range: { start: compiler.bytePosition(document, item.start), end: compiler.bytePosition(document, item.end) } };
}

async function validate(ticket) {
  const controller = new AbortController();
  checking = controller;
  const buffers = documents.all().filter(document => URI.parse(document.uri).scheme === 'file')
    .map(document => ({ path: URI.parse(document.uri).fsPath, text: document.getText(), version: document.version, uri: document.uri }));
  const roots = new Map(buffers.map(buffer => [path.dirname(buffer.path), buffer.uri]));
  const reports = new Map();
  let overlays;
  try {
    if (buffers.length) overlays = await compiler.snapshots(buffers);
    for (const [directory, uri] of roots) {
      if (ticket !== generation) return;
      const report = await compiler.check(await settings(uri), directory, overlays.args, controller.signal);
      for (const item of report.diagnostics) {
        const file = path.resolve(directory, item.path);
        const target = URI.file(file).toString();
        const saved = buffers.find(buffer => buffer.path === file);
        // Loader diagnostics may name a file that does not exist or is unreadable.
        const text = saved?.text ?? await fs.readFile(file, 'utf8').catch(() => '');
        const document = TextDocument.create(target, 'loom', saved?.version ?? 0, text);
        const values = reports.get(target) || [];
        const value = diagnostic(document, item);
        if (!values.some(previous => JSON.stringify(previous) === JSON.stringify(value))) values.push(value);
        reports.set(target, values);
      }
      if (report.error) {
        const values = reports.get(uri) || [];
        values.push({ message: report.error, severity: DiagnosticSeverity.Error, source: 'loom', range: { start: { line: 0, character: 0 }, end: { line: 0, character: 0 } } });
        reports.set(uri, values);
      }
    }
    if (ticket !== generation) return;
    lastError = undefined;
    for (const uri of new Set([...published, ...reports.keys(), ...buffers.map(buffer => buffer.uri)])) {
      connection.sendDiagnostics({ uri, version: documents.get(uri)?.version, diagnostics: reports.get(uri) || [] });
    }
    published.clear();
    for (const uri of reports.keys()) published.add(uri);
  } catch (error) {
    if (ticket === generation && !controller.signal.aborted) {
      for (const uri of published) connection.sendDiagnostics({ uri, diagnostics: [] });
      published.clear();
      if (lastError !== error.message) connection.window.showErrorMessage(`Loom: ${error.message}`);
      lastError = error.message;
    }
  } finally {
    await overlays?.dispose();
    if (checking === controller) checking = undefined;
  }
}

documents.onDidChangeContent(schedule);
documents.onDidClose(schedule);
connection.onDidChangeWatchedFiles(schedule);
connection.onDidChangeConfiguration(change => { defaults = change.settings?.loom || {}; schedule(); });
connection.onDocumentFormatting(async (params, token) => {
  const document = documents.get(params.textDocument.uri);
  if (!document) return [];
  const version = document.version;
  const text = document.getText();
  const controller = new AbortController();
  const cancellation = token.onCancellationRequested(() => controller.abort());
  try {
    const formatted = await compiler.format(await settings(document.uri), path.dirname(URI.parse(document.uri).fsPath), text, controller.signal);
    if (token.isCancellationRequested || documents.get(document.uri)?.version !== version) return [];
    return formatted === text ? [] : [{ range: { start: { line: 0, character: 0 }, end: document.positionAt(text.length) }, newText: formatted }];
  } catch (error) {
    if (token.isCancellationRequested) return [];
    throw new ResponseError(LSPErrorCodes.RequestFailed, error.message);
  } finally { cancellation.dispose(); }
});
connection.onShutdown(async () => { clearTimeout(timer); checking?.abort(); await Promise.allSettled(pending); });
documents.listen(connection);
connection.listen();

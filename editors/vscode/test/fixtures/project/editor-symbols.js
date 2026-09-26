// Transport fixture only. The compiler tests semantic identity separately.
'use strict';
const fs = require('node:fs');
const args = process.argv.slice(2);
const at = args.indexOf('--at');
const file = args[at + 1], offset = Number(args[at + 2]);
const buffers = new Map();
for (let index = 0; index < args.length; index++) {
  if (args[index] !== '--overlay') continue;
  buffers.set(args[++index], fs.readFileSync(args[++index], 'utf8'));
}
const text = buffers.get(file);
const target = [...buffers.keys()].find(path => path.endsWith('query_target.loom'));
const selected = Buffer.from(text).subarray(offset, offset + 5).toString() === 'QUERY';
const declaration = target ? { path: target, start: Buffer.byteLength('// é😀\nfn '), end: Buffer.byteLength('// é😀\nfn chosen') } : null;
const references = selected && declaration ? [declaration, { path: file, start: offset, end: offset + 5 }] : [];
module.exports = mode => {
  const report = mode === 'rename' ? { diagnostics: [], edits: references } : { diagnostics: [], declaration, references };
  setTimeout(() => { process.stdout.write(JSON.stringify(report)); }, text.includes('SLOW') ? 1000 : 0);
};

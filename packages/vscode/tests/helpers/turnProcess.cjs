const { buildSync } = require('esbuild');
const path = require('node:path');
const source = buildSync({
  entryPoints: [path.join(__dirname, '../../../shared/turnProcess.js')],
  bundle: true, write: false, platform: 'node', format: 'cjs',
}).outputFiles[0].text;
const value = { exports: {} };
new Function('module', 'exports', source)(value, value.exports);
module.exports = value.exports;

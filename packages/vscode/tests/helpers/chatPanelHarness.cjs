const { buildSync } = require('esbuild');
const vm = require('node:vm');
const path = require('node:path');

function loadPanel(fetch) {
  const source = buildSync({ entryPoints: [path.join(__dirname, '../../src/chatPanel.ts')], bundle: true, write: false, platform: 'node', format: 'cjs', external: ['vscode'] }).outputFiles[0].text;
  const module = { exports: {} };
  const config = { serverUrl: 'ws://localhost:4096/ws' };
  const vscode = {
    EventEmitter: class { event() {} fire() {} dispose() {} },
    workspace: { getConfiguration: () => ({ get: (key, fallback) => config[key] ?? fallback }), workspaceFolders: [] },
  };
  vm.runInNewContext(source, { module, exports: module.exports, require: name => name === 'vscode' ? vscode : require(name), URL, AbortSignal, fetch, setTimeout, clearTimeout, console, process, Buffer });
  const listeners = {};
  const sent = [];
  const connection = { sessionId: 'a', connected: true, on: (name, fn) => { listeners[name] = fn; }, ensureSession() {}, send: (text, options) => { sent.push({ text, ...options }); return 'operation'; } };
  const saved = new Map();
  const storage = { get: key => saved.get(key), update: async (key, value) => saved.set(key, value) };
  const panel = new module.exports.ChatPanelProvider({}, connection, undefined, storage);
  const messages = [];
  panel.postMessage = msg => messages.push(msg);
  panel.displayedSessionId = 'a';
  return { panel, connection, messages, saved, sent, listeners, config };
}
module.exports = { loadPanel };

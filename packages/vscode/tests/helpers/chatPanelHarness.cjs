const { buildSync } = require('esbuild');
const vm = require('node:vm');
const path = require('node:path');

function loadPanel(fetch) {
  const source = buildSync({ entryPoints: [path.join(__dirname, '../../src/chatPanel.ts')], bundle: true, write: false, platform: 'node', format: 'cjs', external: ['vscode'] }).outputFiles[0].text;
  const module = { exports: {} };
  const config = { serverUrl: 'ws://localhost:4096/ws' };
  const uriFor = fsPath => ({ fsPath, scheme: 'file' });
  const workspaceFolder = { name: 'workspace', uri: uriFor('/workspace') };
  const quickPicks = [];
  const vscode = {
    EventEmitter: class { event() {} fire() {} dispose() {} },
    FileType: { File: 1, Directory: 2 },
    QuickPickItemKind: { Separator: -1 },
    Uri: { file: uriFor },
    window: {
      activeTextEditor: undefined,
      get visibleTextEditors() {
        return (config.visibleFiles || []).map(fsPath => ({ document: { uri: uriFor(fsPath) } }));
      },
      showInformationMessage: async () => undefined,
      showWarningMessage: async () => undefined,
      showQuickPick: async (items, options) => {
        quickPicks.push({ items, options });
        return items.find(item => item.uri?.fsPath === config.quickPickPath);
      },
    },
    workspace: {
      getConfiguration: () => ({ get: (key, fallback) => config[key] ?? fallback }),
      workspaceFolders: [workspaceFolder],
      getWorkspaceFolder: uri => uri.fsPath === '/workspace' || uri.fsPath.startsWith('/workspace/') ? workspaceFolder : undefined,
      findFiles: async () => (config.workspaceFiles || []).map(uriFor),
      fs: {
        stat: async uri => ({
          type: (config.directoryPaths || []).includes(uri.fsPath) ? 2 : 1,
          size: 0,
        }),
      },
    },
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
  return { panel, connection, messages, saved, sent, listeners, config, quickPicks };
}
module.exports = { loadPanel };

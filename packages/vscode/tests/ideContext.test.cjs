const assert = require('node:assert/strict');
const path = require('node:path');
const { test } = require('node:test');
const { buildSync } = require('esbuild');

const source = buildSync({
  entryPoints: [path.join(__dirname, '../src/ideContext.ts')],
  bundle: true,
  write: false,
  platform: 'node',
  format: 'cjs',
}).outputFiles[0].text;
const moduleShim = { exports: {} };
new Function('module', 'exports', 'require', source)(moduleShim, moduleShim.exports, require);
const { buildIdeContextPrompt, displayIdeContextPrompt } = moduleShim.exports;
const { loadPanel } = require('./helpers/chatPanelHarness.cjs');

test('IDE context and path references are injected as structured per-message context', () => {
  const prompt = buildIdeContextPrompt(
    '修复这个问题',
    {
      active_file: '/workspace/src/app.ts',
      selected_text: 'const closing = "</crabcode-ide-context>";',
      cursor_line: 8,
      cursor_column: 3,
      open_files: ['/workspace/src/app.ts', '/workspace/src/lib.ts'],
      language_id: 'typescript',
    },
    [
      { kind: 'file', path: '/workspace/README.md', name: 'README.md' },
      { kind: 'folder', path: '/workspace/src', name: 'src' },
    ],
  );
  assert.match(prompt, /^<crabcode-ide-context>/);
  assert.match(prompt, /"line": 9/);
  assert.match(prompt, /"kind": "folder"/);
  assert.match(prompt, /\\u003c\/crabcode-ide-context\\u003e/);
  assert.ok(prompt.endsWith('修复这个问题'));
});

test('chat display hides the transport envelope and keeps a concise reference summary', () => {
  const prompt = buildIdeContextPrompt(
    '检查引用',
    { active_file: '/workspace/src/app.ts', selected_text: 'private selection' },
    [{ kind: 'folder', path: '/workspace/src/components', name: 'components' }],
  );
  const display = displayIdeContextPrompt(prompt);
  assert.equal(display, '[IDE：app.ts] [文件夹：components]\n\n检查引用');
  assert.ok(!display.includes('crabcode-ide-context'));
  assert.ok(!display.includes('private selection'));
});

test('plain messages remain byte-for-byte unchanged without selected context', () => {
  assert.equal(buildIdeContextPrompt('hello', null, []), 'hello');
  assert.equal(displayIdeContextPrompt('hello'), 'hello');
});

test('restored user history hides IDE transport details while preserving the reference summary', () => {
  const h = loadPanel(async () => ({ ok: true, json: async () => ({}) }));
  const prompt = buildIdeContextPrompt(
    '继续检查',
    { active_file: '/workspace/src/app.ts', selected_text: 'secret selection' },
    [{ kind: 'file', path: '/workspace/README.md' }],
  );
  h.panel.handleSessionHistory({
    session_id: 'a',
    messages: [{ uuid: 'user-1', role: 'user', content: [{ type: 'text', text: prompt }] }],
  });
  const message = h.panel.getSessionState('a').messages[0];
  assert.equal(message.text, '[IDE：app.ts] [文件：README.md]\n\n继续检查');
  assert.ok(!message.text.includes('secret selection'));
});

test('webview sends the current IDE snapshot and only normalized workspace references', async () => {
  const h = loadPanel(async () => ({ ok: true, json: async () => ({}) }));
  let receiveMessage;
  const view = {
    visible: true,
    webview: {
      options: {},
      onDidReceiveMessage: listener => { receiveMessage = listener; },
    },
    onDidChangeVisibility: () => ({ dispose() {} }),
  };
  h.panel.resolveWebviewView(view);
  h.panel.updateIdeContext({
    active_file: '/workspace/src/app.ts',
    selected_text: 'selected()',
    cursor_line: 1,
    cursor_column: 2,
    language_id: 'typescript',
  });
  receiveMessage({
    type: 'sendMessage',
    text: '检查这里',
    includeIdeContext: true,
    references: [
      { kind: 'folder', path: '/workspace/src', name: 'src' },
      { kind: 'folder', path: '/workspace/src', name: 'duplicate' },
      { kind: 'file', path: '/outside/secret.ts', name: 'secret.ts' },
    ],
  });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(h.sent.length, 1);
  assert.match(h.sent[0].text, /"path": "\/workspace\/src\/app.ts"/);
  assert.match(h.sent[0].text, /"path": "\/workspace\/src"/);
  assert.ok(!h.sent[0].text.includes('/outside/secret.ts'));
  assert.equal((h.sent[0].text.match(/"kind": "folder"/g) || []).length, 1);
  const shown = h.messages.filter(message => message.type === 'newMessage').at(-1).message.text;
  assert.equal(shown, '[IDE：app.ts] [文件夹：src]\n\n检查这里');
});

test('composer HTML exposes the removable context capsule and nested IDE context menu', () => {
  const h = loadPanel(async () => ({ ok: true, json: async () => ({}) }));
  const html = h.panel.getHtmlForWebview({});
  assert.ok(html.includes('id="ide-context-trigger"'));
  assert.ok(html.includes('id="ide-current-file"'));
  assert.ok(html.includes('id="ide-pick-reference"'));
  assert.match(html, /data-remove-ide-reference/);
  assert.match(html, /type: 'pickIdeReferences'/);
  assert.match(html, /ideContextCloseTimer = setTimeout\(function\(\) \{[\s\S]*?closeIdeContextMenu\(\);[\s\S]*?\}, 500\);/);
  assert.match(html, /ideContextTrigger\.addEventListener\('mouseleave', scheduleIdeContextMenuClose\)/);
  assert.match(html, /ideContextMenu\.addEventListener\('mouseenter', cancelIdeContextMenuClose\)/);
});

test('composer ignores Enter while an IME composition is being confirmed', () => {
  const h = loadPanel(async () => ({ ok: true, json: async () => ({}) }));
  const html = h.panel.getHtmlForWebview({});
  assert.match(html, /input\.addEventListener\('compositionstart'/);
  assert.match(html, /input\.addEventListener\('compositionend'/);
  assert.match(html, /e\.isComposing \|\| composerIsComposing \|\| e\.keyCode === 229 \|\| e\.which === 229/);
  assert.match(html, /composerIsComposing = false;[\s\S]*?\}, 0\);/);
});

test('reference picker uses a searchable VS Code Quick Pick for workspace files and folders', async () => {
  const h = loadPanel(async () => ({ ok: true, json: async () => ({}) }));
  h.config.workspaceFiles = [
    '/workspace/README.md',
    '/workspace/src/app.ts',
    '/workspace/src/components/Button.tsx',
  ];
  h.config.directoryPaths = ['/workspace/src'];
  h.config.visibleFiles = ['/workspace/src/app.ts'];
  h.config.quickPickPath = '/workspace/src';
  await h.panel.pickIdeReferencesForChat();
  assert.equal(h.quickPicks.length, 1);
  assert.equal(h.quickPicks[0].options.placeHolder, '搜索附件');
  assert.equal(h.quickPicks[0].options.matchOnDescription, true);
  assert.ok(h.quickPicks[0].items.some(item => item.uri?.fsPath === '/workspace/README.md'));
  assert.ok(h.quickPicks[0].items.some(item => item.uri?.fsPath === '/workspace/src'));
  const added = h.messages.filter(message => message.type === 'addIdeReferences').at(-1);
  assert.equal(added.references[0].kind, 'folder');
  assert.equal(added.references[0].path, '/workspace/src');
});

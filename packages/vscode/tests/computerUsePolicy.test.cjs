const assert = require('node:assert/strict');
const { test } = require('node:test');
const { buildSync } = require('esbuild');
const path = require('node:path');
const vm = require('node:vm');

const source = buildSync({
  entryPoints: [path.join(__dirname, '../src/connection.ts')], bundle: true,
  write: false, platform: 'node', format: 'cjs', external: ['vscode', 'ws'],
}).outputFiles[0].text;

function connection(preferences) {
  const module = { exports: {} };
  vm.runInNewContext(source, {
    module, exports: module.exports,
    require: name => name === 'vscode' ? {} : require(name),
    setTimeout, clearTimeout, console, process, Buffer,
  });
  const conn = new module.exports.CrabCodeConnection({
    get: (_key, fallback) => fallback,
    inspect: key => preferences[key] ?? { defaultValue: 'strict_background' },
  });
  const messages = [];
  conn.sendRaw = raw => messages.push(JSON.parse(raw));
  conn.sendCommand = cmd => messages.push(JSON.parse(JSON.stringify(cmd)));
  return { conn, messages };
}

test('explicit scope and delivery permission cross create, resume and send', () => {
  const { conn, messages } = connection({
    computerUseMode: { globalValue: 'foreground_desktop' },
    computerUseTargetScope: { workspaceValue: 'app_window' },
    computerUseDeliveryPolicy: { globalValue: 'allow_foreground' },
  });
  conn.sendNewSession('/workspace');
  conn.sendResumeSession('s');
  conn.send('test', { sessionId: 's' });
  assert.equal(messages.length, 3);
  for (const message of messages) {
    assert.equal(message.computer_use_target_scope, 'app_window');
    assert.equal(message.computer_use_delivery_policy, 'allow_foreground');
    assert.equal(message.computer_use_mode, undefined);
  }
});

test('extension defaults and legacy target do not grant foreground permission', () => {
  const { conn, messages } = connection({ computerUseMode: { globalValue: 'foreground_desktop' } });
  conn.sendNewSession('/workspace');
  conn.send('test', { sessionId: 's' });
  for (const message of messages) {
    assert.equal(message.computer_use_mode, 'foreground_desktop');
    assert.equal(message.computer_use_delivery_policy, undefined);
    assert.equal(message.computer_use_target_scope, undefined);
  }
});

test('a stricter workspace permission overrides a global foreground grant', () => {
  const { conn, messages } = connection({
    computerUseDeliveryPolicy: { globalValue: 'allow_foreground', workspaceValue: 'strict_background' },
  });
  conn.sendResumeSession('s');
  assert.equal(messages[0].computer_use_delivery_policy, 'strict_background');
});

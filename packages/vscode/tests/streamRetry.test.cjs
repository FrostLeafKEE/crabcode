const assert = require('node:assert/strict');
const { test } = require('node:test');
const { loadPanel } = require('./helpers/chatPanelHarness.cjs');

test('stream retry remains busy, shows reconnect status, and starts a fresh response', () => {
  const { panel, messages } = loadPanel(async () => ({ ok: true, json: async () => ({}) }));

  panel.handleServerEvent({
    type: 'stream_text',
    session_id: 'a',
    operation_id: 'op',
    operation_scope: 'foreground',
    text: 'partial',
  });
  panel.handleServerEvent({
    type: 'stream_retry',
    session_id: 'a',
    operation_id: 'op',
    operation_scope: 'foreground',
    message: 'Reconnecting... 1/5',
    error: 'incomplete chunked read',
    retry_count: 1,
    max_retries: 5,
    delay_seconds: 0.2,
    unbounded: false,
    transport_fallback: false,
    discarded_text_chars: 7,
  });

  const state = panel.getSessionState('a');
  assert.equal(state.isBusy, true);
  assert.equal(state.messages.length, 1);
  assert.equal(state.messages[0].text, 'partial');
  assert.ok(messages.some(message => message.type === 'activityStatus' && message.label === 'Reconnecting... 1/5'));

  panel.handleServerEvent({
    type: 'stream_text',
    session_id: 'a',
    operation_id: 'op',
    operation_scope: 'foreground',
    text: 'recovered',
  });

  assert.equal(state.messages.length, 2);
  assert.equal(state.messages[1].text, 'recovered');
});

const { test } = require('node:test');
const assert = require('node:assert/strict');
const net = require('node:net');
const { once } = require('node:events');
const { request } = require('../../shared/computerUse/nativeClient.cjs');

async function server(handler, run) {
  const sockets = new Set();
  const listener = net.createServer(socket => {
    sockets.add(socket);
    socket.on('close', () => sockets.delete(socket));
    let input = '';
    socket.on('data', chunk => {
      input += chunk.toString();
      if (input.endsWith('\n')) handler(socket, JSON.parse(input));
    });
  });
  listener.listen(0, '127.0.0.1');
  await once(listener, 'listening');
  try { await run({ port: listener.address().port, token: 'a'.repeat(64), instance_id: 'i' }); }
  finally { for (const socket of sockets) socket.destroy(); listener.close(); await once(listener, 'close'); }
}

test('native IPC preserves large screenshots, AX metadata and receipt fields', async () => {
  const result = { ok: true, screenshot: { data: 'a'.repeat(256 * 1024), media_type: 'image/png' },
    accessibility: { snapshot_id: 's', elements: [{ element_id: 'e' }] },
    action_dispatched: true, effect_verified: false, focus_isolation: 'preserved' };
  await server((socket, message) => {
    assert.equal(message.method, 'execute');
    assert.equal(message.params.instanceId, 'i');
    const bytes = Buffer.from(JSON.stringify({ ok: true, result }) + '\n');
    for (let i = 0; i < bytes.length; i += 1703) socket.write(bytes.subarray(i, i + 1703));
  }, async endpoint => assert.deepEqual(await request(endpoint, 'execute', { instanceId: 'i' }), result));
});

test('an interrupted native reply never resends input', async () => {
  let count = 0;
  await server(socket => { count++; socket.end('{"ok":true,'); }, async endpoint => {
    await assert.rejects(request(endpoint, 'execute', {}), /不完整/);
    assert.equal(count, 1);
  });
});

test('native IPC times out a nonresponsive executor without retrying', async () => {
  let count = 0;
  await server(() => { count++; }, async endpoint => {
    await assert.rejects(request(endpoint, 'execute', {}, 30), /超时/);
    assert.equal(count, 1);
  });
});

test('native executor errors propagate without converting them into successful receipts', async () => {
  await server(socket => socket.end(JSON.stringify({ ok: false, error: 'stale instance' }) + '\n'), async endpoint => {
    await assert.rejects(request(endpoint, 'execute', {}), /stale instance/);
  });
});

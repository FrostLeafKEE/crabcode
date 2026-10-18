const { test } = require('node:test');
const assert = require('node:assert/strict');

test('missing recording permission is visible with working AX and clears after refresh', async () => {
  const { VSCodeComputerUseHost } = await import('../../computer-use/client/vscodeHost.js');
  const host = new VSCodeComputerUseHost({}, null, '/unused');
  host.panel = { webview: {} };
  host.statusPanel = { webview: {} };
  host.state = { ...host.state, status: 'ready', enabled: true,
    capabilities: { platform: 'macos', input_available: true, ax_available: true, capture_available: false } };
  host.render();
  assert.equal(host.statusLabel, '有限可用');
  for (const panel of [host.panel, host.statusPanel]) {
    assert.match(panel.webview.html, /窗口列表和截图需要录屏权限/);
    assert.match(panel.webview.html, /Crab Desktop 或 VS Code 的授权不能代替此组件的授权/);
    assert.match(panel.webview.html, /~\/Library\/Application Support\/CrabCode\/ComputerUse\/Crab Computer Use\.app/);
  }
  host.state.capabilities.capture_available = true;
  host.render();
  assert.equal(host.statusLabel, '可用');
  for (const panel of [host.panel, host.statusPanel]) {
    assert.doesNotMatch(panel.webview.html, /窗口列表和截图需要录屏权限/);
  }
});

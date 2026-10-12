import { nodeComputerUseChannel } from './nodeChannel.js';
import { initialComputerUseState } from './channel.js';

const escape = value => String(value ?? '').replace(/[&<>"']/g, char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]);
const labels = { disabled: '已关闭', connecting: '正在连接', unavailable: '不可用', ready: '可用', busy: '正在操作', error: '错误' };

/** UI host shared by the ordinary extension and the local Remote SSH companion. */
export class VSCodeComputerUseHost {
  constructor(vscode, WebSocket, bundleDirectory, commandId) {
    this.vscode = vscode;
    this.WebSocket = WebSocket;
    this.bundleDirectory = bundleDirectory;
    this.status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 89);
    this.status.command = commandId;
    this.command = vscode.commands.registerCommand(commandId, () => this.menu());
    this.status.show();
    this.state = initialComputerUseState('', false);
    this.render();
  }

  configure(options) {
    const identity = JSON.stringify([options.gatewayUrl, options.password, options.hostId]);
    if (!this.runtime || this.identity !== identity) {
      this.runtime?.channel.dispose();
      this.identity = identity;
      this.state = initialComputerUseState(options.hostId, options.enabled);
      this.runtime = nodeComputerUseChannel({ ...options, WebSocket: this.WebSocket, bundleDirectory: this.bundleDirectory,
        onState: state => { this.state = state; this.render(); } });
      void this.runtime.channel.connect();
    } else this.runtime.channel.setEnabled(options.enabled);
    this.render();
    return { hostId: options.hostId, enabled: options.enabled };
  }

  render() {
    const limited = this.state.status === 'ready' && (this.state.capabilities?.input_available === false || this.state.capabilities?.capture_available === false);
    const label = limited ? '有限可用' : labels[this.state.status];
    this.status.text = `$(device-desktop) Computer Use · ${label}`;
    this.status.tooltip = this.state.error || '查看 Computer Use 状态、权限和操作记录';
    if (!this.panel) return;
    const previews = this.state.previews.map(preview => {
      const frame = preview.frame;
      const safeFrame = frame && ['image/png', 'image/jpeg', 'image/webp'].includes(frame.media_type) && /^[A-Za-z0-9+/=\r\n]*$/.test(frame.data);
      return `<article><h3>${escape(preview.sessionId)} · ${escape(preview.agentId || '主 Agent')}</h3>
        <p>${escape(preview.summary)} · ${preview.deliveryPolicy === 'strict_background' ? '严格后台' : '允许前台'}</p>
        ${safeFrame ? `<img alt="Computer Use 监看截图" src="data:${frame.media_type};base64,${frame.data}" />` : '<p>暂无监看截图</p>'}
        ${preview.observationKind?.includes('ax') ? '<p>本次模型读取 AX Tree；上图仅供监看。</p>' : ''}</article>`;
    }).join('');
    this.panel.webview.html = `<!doctype html><html lang="zh-CN"><head><meta charset="utf-8">
      <meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline';">
      <style>body{font-family:var(--vscode-font-family);padding:16px;color:var(--vscode-foreground)}article{border:1px solid var(--vscode-panel-border);padding:12px;margin:12px 0}img{max-width:100%;max-height:65vh;object-fit:contain}li{margin:8px 0}p{overflow-wrap:anywhere}</style></head>
      <body><h2>Computer Use · ${label}</h2><p>${escape(this.state.error || '等待 Agent 开始操作电脑')}</p>
      ${this.state.capabilities?.platform === 'macos' && this.state.capabilities.input_available === false ? '<p>请在系统设置 → 隐私与安全性 → 辅助功能中允许 Crab Computer Use；截图另需录屏权限。授权后点击状态栏重新检测。</p>' : ''}
      ${previews}<h3>操作记录</h3><ul>${[...this.state.logs].reverse().map(entry => `<li>${escape(new Date(entry.time).toLocaleTimeString())} · ${escape(entry.summary)}</li>`).join('')}</ul></body></html>`;
  }

  async menu() {
    const choice = await this.vscode.window.showQuickPick([
      { label: this.state.enabled ? '关闭 Computer Use' : '启用 Computer Use', action: 'toggle' },
      { label: '查看监看截图和操作记录', action: 'monitor' },
      { label: '重新检测组件与权限', action: 'refresh' },
      { label: '打开系统权限设置', action: 'permissions' },
    ], { title: `Computer Use · ${labels[this.state.status]}` });
    if (!choice) return;
    try {
      if (choice.action === 'toggle') {
        const config = this.vscode.workspace.getConfiguration('crabcode');
        const target = config.inspect('computerUseEnabled')?.workspaceValue !== undefined
          ? this.vscode.ConfigurationTarget.Workspace : this.vscode.ConfigurationTarget.Global;
        await config.update('computerUseEnabled', !this.state.enabled, target);
      } else if (choice.action === 'refresh') this.runtime?.channel.refresh();
      else if (choice.action === 'permissions') await this.runtime?.openPermissions();
      else if (choice.action === 'monitor') {
        if (this.panel) return this.panel.reveal();
        this.panel = this.vscode.window.createWebviewPanel('crabcode.computerUseMonitor', 'Computer Use', this.vscode.ViewColumn.Beside, { enableScripts: false });
        this.panel.onDidDispose(() => { this.panel = null; });
        this.render();
      }
    } catch (error) { void this.vscode.window.showErrorMessage(String(error.message || error)); }
  }

  dispose() {
    this.runtime?.channel.dispose();
    this.panel?.dispose();
    this.status.dispose();
    this.command.dispose();
  }
}

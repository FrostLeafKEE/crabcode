import * as vscode from 'vscode';
import WebSocket from 'ws';
import path from 'node:path';
import { VSCodeComputerUseHost } from '../../computer-use/client/vscodeHost.js';

export function activate(context) {
  let host;
  let hostId;
  context.subscriptions.push(vscode.commands.registerCommand('crabcode.computerUse.configureLocal', options => {
    if (!host) host = new VSCodeComputerUseHost(vscode, WebSocket, path.join(context.extensionPath, 'resources', 'computer-use'), 'crabcode.computerUse.local');
    hostId = options.hostId;
    return host.configure(options);
  }));
  context.subscriptions.push(vscode.commands.registerCommand('crabcode.computerUse.disposeLocal', id => {
    if (id === hostId) { host?.dispose(); host = undefined; }
  }));
  context.subscriptions.push({ dispose: () => host?.dispose() });
}

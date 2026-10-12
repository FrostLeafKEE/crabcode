import { randomUUID } from 'node:crypto';
import { ComputerUseChannel } from './channel.js';
import native from './nativeClient.cjs';

/** Uses exactly the Desktop state machine, with Node sockets and native IPC. */
export function nodeComputerUseChannel({ gatewayUrl, password, bundleDirectory, hostId, enabled, onState, WebSocket, stateDir }) {
  const client = new native.NativeComputerUse(bundleDirectory, stateDir);
  const url = new URL(gatewayUrl);
  url.pathname = /\/ws\/?$/.test(url.pathname) ? url.pathname.replace(/\/ws\/?$/, '/computer-use/ws')
    : `${url.pathname.replace(/\/$/, '')}/computer-use/ws`;
  url.hash = '';
  const channel = new ComputerUseChannel({
    authenticate: async () => {},
    computerUseWebSocketUrl: () => url.toString(),
  }, hostId, enabled, onState, {
    isNative: () => true,
    platform: process.platform,
    randomUuid: randomUUID,
    invoke: (command, args) => client.invoke(command, args),
    createSocket: address => new WebSocket(address, { headers: password ? { Authorization: `Bearer ${password}` } : {} }),
    setTimeout: (callback, delay) => setTimeout(callback, delay),
    clearTimeout: timer => clearTimeout(timer),
  });
  return { channel, openPermissions: () => client.invoke('computer_use_open_input_settings') };
}

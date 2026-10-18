import type { ComputerUseChannel, ComputerUseState } from './channel.js';
export function nodeComputerUseChannel(options: {
  gatewayUrl: string;
  password?: string;
  bundleDirectory: string;
  hostId: string;
  enabled: boolean;
  onState(state: ComputerUseState): void;
  WebSocket: any;
  stateDir?: string;
}): { channel: ComputerUseChannel; openPermissions(kind?: 'screen_recording' | 'accessibility'): Promise<unknown> };

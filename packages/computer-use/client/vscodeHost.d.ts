export interface ComputerUseHostOptions { gatewayUrl: string; password?: string; hostId: string; enabled: boolean }
export interface ComputerUseBinding { hostId: string; enabled: boolean }
export class VSCodeComputerUseHost {
  constructor(vscode: any, WebSocket: any, bundleDirectory: string);
  configure(options: ComputerUseHostOptions): ComputerUseBinding;
  menu(): Promise<void>;
  dispose(): void;
}

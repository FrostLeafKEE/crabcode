export class NativeComputerUse {
  constructor(bundleDirectory: string, stateDir?: string);
  start(): Promise<void>;
  invoke<T = unknown>(command: string, args?: Record<string, unknown>): Promise<T>;
}
export function stateDirectory(): string;
export function bundledExecutable(directory: string): string;

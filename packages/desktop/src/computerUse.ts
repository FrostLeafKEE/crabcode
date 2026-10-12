import { invoke } from "@tauri-apps/api/core";
import { isDesktopShell } from "./native";
import { randomUuid } from "./uuid";
import type { GatewayApi } from "./gateway";
import type { LocalVmConfig } from "./virtualMachine";
import { ComputerUseChannel as SharedComputerUseChannel, type ComputerUseState } from "../../shared/computerUse/channel.js";
export * from "../../shared/computerUse/channel.js";
const COMPUTER_USE_HOST_ID_STORAGE_KEY = "crabcode.computer-use-host-id";

export function computerUseHostId(): string {
  // sessionStorage survives a webview reload but remains isolated per Desktop
  // window.  Reusing the id lets the Gateway replay leases owned before the
  // reload without merging independent app instances.
  try {
    const stored = window.sessionStorage.getItem(COMPUTER_USE_HOST_ID_STORAGE_KEY);
    if (stored && /^desktop-[A-Za-z0-9-]{1,180}$/.test(stored)) return stored;
    const created = `desktop-${randomUuid()}`;
    window.sessionStorage.setItem(COMPUTER_USE_HOST_ID_STORAGE_KEY, created);
    return created;
  } catch {
    return `desktop-${randomUuid()}`;
  }
}

export async function openComputerUseInputSettings(): Promise<void> {
  if (!isDesktopShell()) throw new Error("Accessibility settings require the native Computer Use component");
  await invoke("computer_use_open_input_settings");
}

export class ComputerUseChannel extends SharedComputerUseChannel {
  constructor(api: GatewayApi, hostId: string, enabled: boolean, onState: (state: ComputerUseState) => void, vm: LocalVmConfig | null = null) {
    super(api, hostId, enabled, onState, {
      isNative: isDesktopShell,
      platform: navigator.platform || "unknown",
      randomUuid,
      invoke,
      createSocket: (url) => new WebSocket(url),
      setTimeout: (callback, delay) => window.setTimeout(callback, delay),
      clearTimeout: (timer) => window.clearTimeout(timer),
    }, vm);
  }
}

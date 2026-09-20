/* @vitest-environment jsdom */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { ComputerUseChannel } from "./computerUse";
import type { GatewayApi } from "./gateway";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("./native", () => ({ isDesktopShell: () => true }));

class FakeWebSocket {
  static OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readyState = 0;
  private listeners = new Map<string, Array<(event: { data?: string }) => void>>();
  constructor(public url: string) { FakeWebSocket.instances.push(this); }
  addEventListener(type: string, listener: (event: { data?: string }) => void) {
    const entries = this.listeners.get(type) ?? [];
    entries.push(listener);
    this.listeners.set(type, entries);
  }
  emit(type: string, event: { data?: string } = {}) {
    if (type === "open") this.readyState = FakeWebSocket.OPEN;
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
  send() {}
  close() {}
}

describe("ComputerUseChannel", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    FakeWebSocket.instances = [];
    vi.stubGlobal("WebSocket", FakeWebSocket);
  });

  it("does not inspect the GUI while disabled and detects it when enabled", async () => {
    invokeMock.mockResolvedValue({
      gui_available: true,
      input_available: true,
      platform: "macos",
      displays: [],
      supported_modes: ["background_app", "foreground_desktop"],
    });
    const api = {
      authenticate: vi.fn().mockResolvedValue(undefined),
      computerUseWebSocketUrl: () => "ws://localhost/computer-use/ws",
    } as unknown as GatewayApi;
    const channel = new ComputerUseChannel(api, "desktop-test", false, vi.fn());

    await channel.connect();
    expect(invokeMock).not.toHaveBeenCalled();

    channel.setEnabled(true);
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("computer_use_capabilities"));
    channel.dispose();
  });

  it("forwards the requested mode to the native executor without fallback", async () => {
    invokeMock
      .mockResolvedValueOnce({
        gui_available: true,
        input_available: true,
        platform: "macos",
        displays: [],
        supported_modes: ["background_app", "foreground_desktop"],
      })
      .mockResolvedValueOnce({ ok: true, action: "list_windows", windows: [] });
    const api = {
      authenticate: vi.fn().mockResolvedValue(undefined),
      computerUseWebSocketUrl: () => "ws://localhost/computer-use/ws",
    } as unknown as GatewayApi;
    const channel = new ComputerUseChannel(api, "desktop-test", true, vi.fn());

    await channel.connect();
    const socket = FakeWebSocket.instances[0];
    socket.emit("open");
    socket.emit("message", {
      data: JSON.stringify({
        type: "computer_use_request",
        request_id: "request-1",
        mode: "background_app",
        action: { action: "list_windows" },
      }),
    });

    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("computer_use_execute", {
      request: { mode: "background_app", action: { action: "list_windows" } },
    }));
    channel.dispose();
  });
});

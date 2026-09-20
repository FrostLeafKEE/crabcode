/* @vitest-environment jsdom */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { COMPUTER_USE_RELEASE_RETENTION_MS, ComputerUseChannel, computerUseHostId } from "./computerUse";
import type { ComputerUseState } from "./computerUse";
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
    window.sessionStorage.clear();
  });

  it("keeps one host id in the current webview across reloads", () => {
    const first = computerUseHostId();
    const second = computerUseHostId();
    expect(second).toBe(first);
    expect(first).toMatch(/^desktop-/);
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

  it("retains the preview while idle and releases it 15 seconds after the session releases Computer Use", async () => {
    vi.useFakeTimers();
    try {
      invokeMock
        .mockResolvedValueOnce({
          gui_available: true,
          input_available: true,
          platform: "macos",
          displays: [],
          supported_modes: ["background_app", "foreground_desktop"],
        })
        .mockResolvedValueOnce({
          ok: true,
          action: "observe",
          screenshot: {
            data: "cG5n",
            media_type: "image/png",
            width: 100,
            height: 50,
            origin_x: 0,
            origin_y: 0,
            frame_id: "frame-1",
          },
          cursor: { x: 10, y: 10 },
        });
      const states: ComputerUseState[] = [];
      const api = {
        authenticate: vi.fn().mockResolvedValue(undefined),
        computerUseWebSocketUrl: () => "ws://localhost/computer-use/ws",
      } as unknown as GatewayApi;
      const channel = new ComputerUseChannel(api, "desktop-test", true, (state) => states.push(state));

      await channel.connect();
      const socket = FakeWebSocket.instances[0];
      socket.emit("open");
      socket.emit("message", {
        data: JSON.stringify({
          type: "computer_use_request",
          request_id: "request-idle",
          session_id: "session-idle",
          mode: "background_app",
          action: { action: "observe", window_id: "42" },
        }),
      });
      await vi.advanceTimersByTimeAsync(0);

      expect(states.at(-1)).toMatchObject({ active: true, status: "ready" });
      expect(states.at(-1)?.previews[0]?.frame?.frame_id).toBe("frame-1");
      await vi.advanceTimersByTimeAsync(COMPUTER_USE_RELEASE_RETENTION_MS * 2);
      expect(states.at(-1)).toMatchObject({ active: true });
      socket.emit("message", {
        data: JSON.stringify({
          type: "computer_use_release",
          session_id: "session-idle",
        }),
      });
      await vi.advanceTimersByTimeAsync(COMPUTER_USE_RELEASE_RETENTION_MS);
      expect(states.at(-1)).toMatchObject({ active: false, previews: [] });
      expect(states.at(-1)?.logs).toHaveLength(1);
      channel.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("keeps simultaneous sessions in independent preview slots and releases them independently", async () => {
    vi.useFakeTimers();
    try {
      invokeMock
        .mockResolvedValueOnce({
          gui_available: true,
          input_available: true,
          platform: "macos",
          displays: [],
          supported_modes: ["background_app", "foreground_desktop"],
        })
        .mockResolvedValueOnce({
          ok: true,
          action: "observe",
          screenshot: { data: "MQ==", media_type: "image/png", width: 10, height: 10, origin_x: 0, origin_y: 0, frame_id: "frame-one" },
        })
        .mockResolvedValueOnce({
          ok: true,
          action: "observe",
          screenshot: { data: "Mg==", media_type: "image/png", width: 10, height: 10, origin_x: 0, origin_y: 0, frame_id: "frame-two" },
        });
      const states: ComputerUseState[] = [];
      const api = {
        authenticate: vi.fn().mockResolvedValue(undefined),
        computerUseWebSocketUrl: () => "ws://localhost/computer-use/ws",
      } as unknown as GatewayApi;
      const channel = new ComputerUseChannel(api, "desktop-test", true, (state) => states.push(state));

      await channel.connect();
      const socket = FakeWebSocket.instances[0];
      socket.emit("open");
      socket.emit("message", {
        data: JSON.stringify({ type: "computer_use_request", request_id: "one", session_id: "session-one", action: { action: "observe" } }),
      });
      await vi.advanceTimersByTimeAsync(0);
      await vi.advanceTimersByTimeAsync(5_000);
      socket.emit("message", {
        data: JSON.stringify({ type: "computer_use_request", request_id: "two", session_id: "session-two", action: { action: "observe" } }),
      });
      await vi.advanceTimersByTimeAsync(0);

      expect(states.at(-1)?.previews.map((preview) => preview.frame?.frame_id)).toEqual(["frame-one", "frame-two"]);
      await vi.advanceTimersByTimeAsync(COMPUTER_USE_RELEASE_RETENTION_MS * 2);
      expect(states.at(-1)?.previews.map((preview) => preview.frame?.frame_id)).toEqual(["frame-one", "frame-two"]);
      socket.emit("message", {
        data: JSON.stringify({ type: "computer_use_release", session_id: "session-one" }),
      });
      await vi.advanceTimersByTimeAsync(5_000);
      socket.emit("message", {
        data: JSON.stringify({ type: "computer_use_release", session_id: "session-two" }),
      });
      await vi.advanceTimersByTimeAsync(10_000);
      expect(states.at(-1)?.previews.map((preview) => preview.frame?.frame_id)).toEqual(["frame-two"]);
      await vi.advanceTimersByTimeAsync(5_000);
      expect(states.at(-1)).toMatchObject({ active: false, previews: [] });
      channel.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("restores an active preview after reconnect and preserves its release deadline", async () => {
    vi.useFakeTimers();
    try {
      invokeMock.mockResolvedValueOnce({
        gui_available: true,
        input_available: true,
        platform: "macos",
        displays: [],
        supported_modes: ["background_app", "foreground_desktop"],
      });
      const states: ComputerUseState[] = [];
      const api = {
        authenticate: vi.fn().mockResolvedValue(undefined),
        computerUseWebSocketUrl: () => "ws://localhost/computer-use/ws",
      } as unknown as GatewayApi;
      const channel = new ComputerUseChannel(api, "desktop-test", true, (state) => states.push(state));

      await channel.connect();
      const socket = FakeWebSocket.instances[0];
      socket.emit("open");
      socket.emit("message", {
        data: JSON.stringify({
          type: "computer_use_host_registered",
          available: true,
          previews: [{
            session_id: "session-restored",
            mode: "background_app",
            status: "ready",
            action: "observe",
            summary: "Observed window",
            frame: { data: "MQ==", media_type: "image/png", width: 10, height: 10, origin_x: 0, origin_y: 0, frame_id: "restored-frame" },
            release_deadline_ms: Date.now() + COMPUTER_USE_RELEASE_RETENTION_MS,
          }],
        }),
      });

      expect(states.at(-1)).toMatchObject({ active: true });
      expect(states.at(-1)?.previews[0]?.frame?.frame_id).toBe("restored-frame");
      await vi.advanceTimersByTimeAsync(COMPUTER_USE_RELEASE_RETENTION_MS - 1);
      expect(states.at(-1)).toMatchObject({ active: true });
      await vi.advanceTimersByTimeAsync(1);
      expect(states.at(-1)).toMatchObject({ active: false, previews: [] });
      channel.dispose();
    } finally {
      vi.useRealTimers();
    }
  });

  it("expires a released busy preview after 15 seconds and ignores its late native result", async () => {
    vi.useFakeTimers();
    try {
      let finishAction!: (result: object) => void;
      invokeMock
        .mockResolvedValueOnce({
          gui_available: true,
          input_available: true,
          platform: "macos",
          displays: [],
          supported_modes: ["background_app", "foreground_desktop"],
        })
        .mockReturnValueOnce(new Promise((resolve) => { finishAction = resolve; }));
      const states: ComputerUseState[] = [];
      const api = {
        authenticate: vi.fn().mockResolvedValue(undefined),
        computerUseWebSocketUrl: () => "ws://localhost/computer-use/ws",
      } as unknown as GatewayApi;
      const channel = new ComputerUseChannel(api, "desktop-test", true, (state) => states.push(state));

      await channel.connect();
      const socket = FakeWebSocket.instances[0];
      socket.emit("open");
      socket.emit("message", {
        data: JSON.stringify({
          type: "computer_use_request",
          request_id: "request-slow",
          session_id: "session-slow",
          action: { action: "observe" },
        }),
      });
      await vi.advanceTimersByTimeAsync(0);
      expect(states.at(-1)?.previews[0]?.status).toBe("busy");

      socket.emit("message", {
        data: JSON.stringify({ type: "computer_use_release", session_id: "session-slow" }),
      });
      await vi.advanceTimersByTimeAsync(COMPUTER_USE_RELEASE_RETENTION_MS);
      expect(states.at(-1)).toMatchObject({ active: false, previews: [] });

      finishAction({ ok: true, action: "observe" });
      await vi.advanceTimersByTimeAsync(0);
      expect(states.at(-1)).toMatchObject({ active: false, previews: [] });
      channel.dispose();
    } finally {
      vi.useRealTimers();
    }
  });
});

/* @vitest-environment jsdom */

import { beforeEach, describe, expect, it, vi } from "vitest";
import { SessionChannel, type GatewayApi } from "./gateway";

class FakeWebSocket {
  static OPEN = 1;
  static instances: FakeWebSocket[] = [];
  readyState = 0;
  sent: string[] = [];
  private listeners = new Map<string, Array<(event: Event) => void>>();

  constructor(public url: string) {
    FakeWebSocket.instances.push(this);
  }

  addEventListener(type: string, listener: (event: Event) => void) {
    const entries = this.listeners.get(type) ?? [];
    entries.push(listener);
    this.listeners.set(type, entries);
  }

  emit(type: string) {
    if (type === "open") this.readyState = FakeWebSocket.OPEN;
    for (const listener of this.listeners.get(type) ?? []) listener(new Event(type));
  }

  send(value: string) {
    this.sent.push(value);
  }

  close() {}
}

describe("SessionChannel new-session controls", () => {
  beforeEach(() => {
    FakeWebSocket.instances = [];
    vi.stubGlobal("WebSocket", FakeWebSocket);
  });

  it("sends the remembered composer controls with the initial request", async () => {
    const api = {
      authenticate: vi.fn().mockResolvedValue(undefined),
      webSocketUrl: () => "ws://localhost/ws",
    } as unknown as GatewayApi;
    const channel = new SessionChannel(api, {
      cwd: "/work/crab",
      modelProfile: "smart",
      reasoningEffort: "xhigh",
      ultraMode: true,
      mode: "plan",
      permissionMode: "ai_review",
      onEvent: vi.fn(),
      onReady: vi.fn(),
      onState: vi.fn(),
    });

    await channel.connect();
    const socket = FakeWebSocket.instances[0];
    socket.emit("open");

    expect(JSON.parse(socket.sent[0])).toMatchObject({
      type: "new_session",
      cwd: "/work/crab",
      model_profile: "smart",
      reasoning_effort: "xhigh",
      ultra_mode: true,
      mode: "plan",
      permission_mode: "ai_review",
    });
    channel.dispose();
  });
});

import { invoke } from "@tauri-apps/api/core";
import type { GatewayApi } from "./gateway";
import { isDesktopShell } from "./native";
import { randomUuid } from "./uuid";

export type ComputerUseStatus = "disabled" | "unavailable" | "connecting" | "ready" | "busy" | "error";
export const COMPUTER_USE_IDLE_RELEASE_MS = 15_000;

export interface ComputerUseCapabilities {
  gui_available: boolean;
  input_available: boolean;
  platform: string;
  displays: Array<{ id: string; name: string; x: number; y: number; width: number; height: number; primary: boolean }>;
  supported_modes: Array<"background_app" | "foreground_desktop">;
  reason?: string | null;
}

export interface ComputerUseFrame {
  data: string;
  media_type: string;
  width: number;
  height: number;
  origin_x: number;
  origin_y: number;
  frame_id: string;
}

export interface ComputerUseCursor {
  x: number;
  y: number;
}

export interface ComputerUseLogEntry {
  id: string;
  time: number;
  action: string;
  summary: string;
  ok: boolean;
  sessionId?: string;
  agentId?: string;
}

export interface ComputerUseState {
  hostId: string;
  enabled: boolean;
  active: boolean;
  mode: "background_app" | "foreground_desktop";
  status: ComputerUseStatus;
  capabilities: ComputerUseCapabilities | null;
  latestFrame: ComputerUseFrame | null;
  cursor: ComputerUseCursor | null;
  logs: ComputerUseLogEntry[];
  error: string | null;
}

interface HostResult {
  ok: boolean;
  summary?: string;
  error?: string;
  action?: string;
  screenshot?: ComputerUseFrame;
  cursor?: ComputerUseCursor;
  [key: string]: unknown;
}

export function computerUseHostId(): string {
  // A runtime-scoped id keeps simultaneous Desktop instances isolated. Any
  // resumed session is rebound through its SessionChannel handshake.
  return `desktop-${randomUuid()}`;
}

export function initialComputerUseState(hostId: string, enabled: boolean): ComputerUseState {
  return {
    hostId,
    enabled,
    active: false,
    mode: "background_app",
    status: enabled ? "connecting" : "disabled",
    capabilities: null,
    latestFrame: null,
    cursor: null,
    logs: [],
    error: null,
  };
}

export async function openComputerUseInputSettings(): Promise<void> {
  if (!isDesktopShell()) throw new Error("辅助功能权限设置只能从 Crab Desktop 打开");
  await invoke("computer_use_open_input_settings");
}

export class ComputerUseChannel {
  private socket: WebSocket | null = null;
  private reconnectTimer: number | null = null;
  private attempts = 0;
  private capabilityGeneration = 0;
  private idleReleaseTimer: number | null = null;
  private disposed = false;
  private enabled: boolean;
  private capabilities: ComputerUseCapabilities | null = null;
  private state: ComputerUseState;

  constructor(
    private api: GatewayApi,
    private hostId: string,
    enabled: boolean,
    private onState: (state: ComputerUseState) => void,
  ) {
    this.enabled = enabled;
    this.state = initialComputerUseState(hostId, enabled);
  }

  private publish(update: Partial<ComputerUseState>): void {
    this.state = { ...this.state, ...update, hostId: this.hostId, enabled: this.enabled };
    this.onState(this.state);
  }

  async connect(): Promise<void> {
    if (this.disposed) return;
    this.publish({ status: this.enabled ? "connecting" : "disabled", error: null });
    if (this.enabled) await this.refreshCapabilities();
    if (this.disposed) return;

    try {
      await this.api.authenticate();
      if (this.disposed) return;
      const socket = new WebSocket(this.api.computerUseWebSocketUrl());
      this.socket = socket;
      socket.addEventListener("open", () => {
        if (this.disposed || this.socket !== socket) return socket.close();
        this.attempts = 0;
        this.sendRegistration();
      });
      socket.addEventListener("message", (event) => {
        if (this.disposed || this.socket !== socket) return;
        void this.handleMessage(String(event.data));
      });
      socket.addEventListener("close", () => {
        if (this.socket !== socket) return;
        this.socket = null;
        if (!this.disposed) {
          this.publish({ status: this.enabled ? "connecting" : "disabled" });
          this.scheduleReconnect();
        }
      });
      socket.addEventListener("error", () => {
        if (this.socket === socket) this.publish({ status: "error", error: "Computer Use Host 连接失败" });
      });
    } catch (error) {
      this.publish({ status: "error", error: error instanceof Error ? error.message : String(error) });
      this.scheduleReconnect();
    }
  }

  private async refreshCapabilities(): Promise<boolean> {
    const generation = ++this.capabilityGeneration;
    let capabilities: ComputerUseCapabilities;
    if (!isDesktopShell()) {
      capabilities = {
        gui_available: false,
        input_available: false,
        platform: "browser",
        displays: [],
        supported_modes: [],
        reason: "Computer Use 需要 Crab Desktop 原生应用",
      };
    } else {
      try {
        capabilities = await invoke<ComputerUseCapabilities>("computer_use_capabilities");
      } catch (error) {
        capabilities = {
          gui_available: false,
          input_available: false,
          platform: navigator.platform || "unknown",
          displays: [],
          supported_modes: [],
          reason: error instanceof Error ? error.message : String(error),
        };
      }
    }
    if (this.disposed || !this.enabled || generation !== this.capabilityGeneration) return false;
    this.capabilities = capabilities;
    this.publish({
      capabilities,
      status: capabilities.gui_available ? (this.socket?.readyState === WebSocket.OPEN ? "ready" : "connecting") : "unavailable",
      error: capabilities.reason ?? null,
    });
    return true;
  }

  setEnabled(enabled: boolean): void {
    const changed = this.enabled !== enabled;
    this.enabled = enabled;
    if (enabled && changed) {
      this.publish({ status: "connecting", error: null });
      void this.refreshCapabilities().then((applied) => {
        if (!applied || !this.enabled || this.disposed) return;
        this.send({
          type: "computer_use_host_state",
          enabled: true,
          gui_available: this.capabilities?.gui_available === true,
          capabilities: this.capabilities,
        });
      });
      return;
    }
    this.publish({
      status: !enabled ? "disabled" : this.capabilities?.gui_available ? (this.socket?.readyState === WebSocket.OPEN ? "ready" : "connecting") : "unavailable",
      ...(!enabled ? { active: false, latestFrame: null, cursor: null } : {}),
      error: enabled ? this.capabilities?.reason ?? null : null,
    });
    if (!enabled) this.cancelIdleRelease();
    this.send({
      type: "computer_use_host_state",
      enabled,
      gui_available: this.capabilities?.gui_available === true,
      capabilities: this.capabilities,
    });
  }

  refresh(): void {
    if (!this.enabled || this.disposed) return;
    this.publish({ status: "connecting", error: null });
    void this.refreshCapabilities().then((applied) => {
      if (!applied || !this.enabled || this.disposed) return;
      this.send({
        type: "computer_use_host_state",
        enabled: true,
        gui_available: this.capabilities?.gui_available === true,
        capabilities: this.capabilities,
      });
    });
  }

  dispose(): void {
    this.disposed = true;
    if (this.reconnectTimer !== null) window.clearTimeout(this.reconnectTimer);
    this.reconnectTimer = null;
    this.cancelIdleRelease();
    this.socket?.close();
    this.socket = null;
  }

  private sendRegistration(): void {
    this.send({
      type: "computer_use_host_register",
      host_id: this.hostId,
      enabled: this.enabled,
      gui_available: this.capabilities?.gui_available === true,
      capabilities: this.capabilities,
    });
  }

  private send(payload: Record<string, unknown>): void {
    if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(JSON.stringify(payload));
  }

  private scheduleReconnect(): void {
    if (this.disposed || this.reconnectTimer !== null) return;
    const delay = Math.min(30_000, 1000 * 2 ** Math.min(this.attempts++, 5));
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      void this.connect();
    }, delay);
  }

  private cancelIdleRelease(): void {
    if (this.idleReleaseTimer !== null) window.clearTimeout(this.idleReleaseTimer);
    this.idleReleaseTimer = null;
  }

  private scheduleIdleRelease(): void {
    this.cancelIdleRelease();
    this.idleReleaseTimer = window.setTimeout(() => {
      this.idleReleaseTimer = null;
      if (this.disposed || this.state.status === "busy") return;
      this.publish({ active: false, latestFrame: null, cursor: null });
    }, COMPUTER_USE_IDLE_RELEASE_MS);
  }

  private async handleMessage(raw: string): Promise<void> {
    let message: Record<string, unknown>;
    try {
      message = JSON.parse(raw) as Record<string, unknown>;
    } catch {
      this.publish({ status: "error", error: "Computer Use Host 收到无效消息" });
      return;
    }
    if (message.type === "computer_use_host_registered" || message.type === "computer_use_host_state_ack") {
      const available = message.available === true;
      this.publish({
        status: !this.enabled ? "disabled" : available ? "ready" : "unavailable",
        error: this.capabilities?.reason ?? null,
      });
      return;
    }
    if (message.type === "computer_use_error") {
      this.publish({ status: "error", error: String(message.error || "Computer Use Host 错误") });
      return;
    }
    if (message.type !== "computer_use_request") return;

    const requestId = String(message.request_id || "");
    const action = message.action && typeof message.action === "object"
      ? message.action as Record<string, unknown>
      : {};
    const actionName = String(action.action || "unknown");
    const mode = message.mode === "foreground_desktop" ? "foreground_desktop" : "background_app";
    const logId = requestId || randomUuid();
    this.cancelIdleRelease();
    this.publish({
      status: "busy",
      active: true,
      mode,
      error: null,
      logs: [...this.state.logs, {
        id: logId,
        time: Date.now(),
        action: actionName,
        summary: "正在执行…",
        ok: true,
        sessionId: typeof message.session_id === "string" ? message.session_id : undefined,
        agentId: typeof message.agent_id === "string" ? message.agent_id : undefined,
      }].slice(-100),
    });
    let result: HostResult;
    try {
      if (!this.enabled || !this.capabilities?.gui_available) throw new Error("Computer Use 已关闭或图形界面不可用");
      result = await invoke<HostResult>("computer_use_execute", { request: { mode, action } });
    } catch (error) {
      result = { ok: false, action: actionName, error: error instanceof Error ? error.message : String(error) };
    }

    const entry: ComputerUseLogEntry = {
      id: logId,
      time: Date.now(),
      action: String(result.action || actionName),
      summary: String(result.summary || result.error || actionName),
      ok: result.ok !== false,
      sessionId: typeof message.session_id === "string" ? message.session_id : undefined,
      agentId: typeof message.agent_id === "string" ? message.agent_id : undefined,
    };
    this.publish({
      status: !this.enabled
        ? "disabled"
        : result.ok === false
          ? "error"
          : this.capabilities?.gui_available ? "ready" : "unavailable",
      latestFrame: this.enabled ? result.screenshot ?? this.state.latestFrame : null,
      cursor: this.enabled ? result.cursor ?? this.state.cursor : null,
      logs: [...this.state.logs.filter((item) => item.id !== logId), entry].slice(-100),
      error: result.ok === false
        ? String(result.error || "Computer Use action failed")
        : this.capabilities?.reason ?? null,
    });
    if (this.enabled) this.scheduleIdleRelease();
    else this.cancelIdleRelease();
    this.send({ type: "computer_use_result", request_id: requestId, result });
  }
}

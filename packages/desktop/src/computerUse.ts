import { invoke } from "@tauri-apps/api/core";
import type { GatewayApi } from "./gateway";
import { isDesktopShell } from "./native";
import { randomUuid } from "./uuid";

export type ComputerUseStatus = "disabled" | "unavailable" | "connecting" | "ready" | "busy" | "error";
export const COMPUTER_USE_RELEASE_RETENTION_MS = 15_000;
const COMPUTER_USE_HOST_ID_STORAGE_KEY = "crabcode.computer-use-host-id";

export interface ComputerUseCapabilities {
  gui_available: boolean;
  input_available: boolean;
  platform: string;
  displays: Array<{ id: string; name: string; x: number; y: number; width: number; height: number; primary: boolean }>;
  supported_modes: Array<"background_app" | "foreground_desktop">;
  delivery_policy_version?: number;
  strict_background_input_available?: boolean;
  strict_background_reason?: string;
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

export interface ComputerUsePreview {
  key: string;
  sessionId?: string;
  agentId?: string;
  mode: "background_app" | "foreground_desktop";
  deliveryPolicy?: "strict_background" | "allow_foreground";
  status: "busy" | "ready" | "error";
  action: string;
  summary: string;
  frame: ComputerUseFrame | null;
  cursor: ComputerUseCursor | null;
  updatedAt: number;
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
  deliveryPolicy?: "strict_background" | "allow_foreground";
  status: ComputerUseStatus;
  capabilities: ComputerUseCapabilities | null;
  previews: ComputerUsePreview[];
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

export function initialComputerUseState(hostId: string, enabled: boolean): ComputerUseState {
  return {
    hostId,
    enabled,
    active: false,
    mode: "background_app",
    deliveryPolicy: "allow_foreground",
    status: enabled ? "connecting" : "disabled",
    capabilities: null,
    previews: [],
    logs: [],
    error: null,
  };
}

export async function openComputerUseInputSettings(): Promise<void> {
  if (!isDesktopShell()) throw new Error("Accessibility settings can only be opened from Crab Desktop");
  await invoke("computer_use_open_input_settings");
}

export class ComputerUseChannel {
  private socket: WebSocket | null = null;
  private reconnectTimer: number | null = null;
  private attempts = 0;
  private capabilityGeneration = 0;
  private releaseTimers = new Map<string, number>();
  private releaseDeadlines = new Map<string, number>();
  private latestRequestIds = new Map<string, string>();
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
        if (this.socket === socket) this.publish({ status: "error", error: "Computer Use Host connection failed" });
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
        reason: "Computer Use requires the Crab Desktop native app",
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
      ...(!enabled ? { active: false, previews: [] } : {}),
      error: enabled ? this.capabilities?.reason ?? null : null,
    });
    if (!enabled) this.cancelAllReleases();
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
    this.cancelAllReleases();
    this.latestRequestIds.clear();
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

  private previewKey(sessionId?: string, agentId?: string): string {
    return `session:${sessionId || "unknown"}:agent:${agentId || "main"}`;
  }

  private cancelRelease(key: string): void {
    const timer = this.releaseTimers.get(key);
    if (timer !== undefined) window.clearTimeout(timer);
    this.releaseTimers.delete(key);
    this.releaseDeadlines.delete(key);
  }

  private cancelAllReleases(): void {
    for (const timer of this.releaseTimers.values()) window.clearTimeout(timer);
    this.releaseTimers.clear();
    this.releaseDeadlines.clear();
  }

  private scheduleRelease(
    key: string,
    deadline = Date.now() + COMPUTER_USE_RELEASE_RETENTION_MS,
  ): void {
    const previousTimer = this.releaseTimers.get(key);
    if (previousTimer !== undefined) window.clearTimeout(previousTimer);
    this.releaseDeadlines.set(key, deadline);
    const timer = window.setTimeout(() => {
      this.releaseTimers.delete(key);
      if (this.disposed) return;
      this.releaseDeadlines.delete(key);
      const previews = this.state.previews.filter((preview) => preview.key !== key);
      const status = previews.some((preview) => preview.status === "busy")
        ? "busy"
        : previews.some((preview) => preview.status === "error")
          ? "error"
        : this.enabled && this.capabilities?.gui_available
          ? "ready"
          : this.state.status;
      this.publish({ active: previews.length > 0, previews, status });
    }, Math.max(0, deadline - Date.now()));
    this.releaseTimers.set(key, timer);
  }

  private restorePreviews(raw: unknown): void {
    if (!this.enabled || !Array.isArray(raw)) return;
    const restored: ComputerUsePreview[] = [];
    const deadlines = new Map<string, number>();
    for (const value of raw) {
      if (!value || typeof value !== "object") continue;
      const item = value as Record<string, unknown>;
      const sessionId = typeof item.session_id === "string" ? item.session_id : undefined;
      if (!sessionId) continue;
      const agentId = typeof item.agent_id === "string" ? item.agent_id : undefined;
      const key = this.previewKey(sessionId, agentId);
      const status = item.status === "busy" || item.status === "error" ? item.status : "ready";
      const deadline = typeof item.release_deadline_ms === "number"
        ? item.release_deadline_ms
        : undefined;
      if (deadline !== undefined && deadline <= Date.now()) continue;
      restored.push({
        key,
        sessionId,
        agentId,
        mode: item.mode === "foreground_desktop" ? "foreground_desktop" : "background_app",
        deliveryPolicy: item.delivery_policy === "allow_foreground" ? "allow_foreground"
          : item.delivery_policy === "strict_background" ? "strict_background" : undefined,
        status,
        action: String(item.action || "unknown"),
        summary: String(item.summary || (status === "busy" ? "Executing…" : item.action || "Computer Use")),
        frame: item.frame && typeof item.frame === "object" ? item.frame as ComputerUseFrame : null,
        cursor: item.cursor && typeof item.cursor === "object" ? item.cursor as ComputerUseCursor : null,
        updatedAt: typeof item.updated_at_ms === "number" ? item.updated_at_ms : Date.now(),
      });
      if (deadline !== undefined) deadlines.set(key, deadline);
    }
    if (restored.length === 0) return;
    const keys = new Set(restored.map((preview) => preview.key));
    const previews = [
      ...this.state.previews.filter((preview) => !keys.has(preview.key)),
      ...restored,
    ];
    const latest = previews.reduce((a, b) => b.updatedAt > a.updatedAt ? b : a);
    this.publish({
      active: true,
      mode: latest.mode,
      deliveryPolicy: latest.deliveryPolicy,
      previews,
      status: previews.some((preview) => preview.status === "busy")
        ? "busy"
        : previews.some((preview) => preview.status === "error")
          ? "error"
          : this.state.status,
    });
    for (const [key, deadline] of deadlines) this.scheduleRelease(key, deadline);
  }

  private async handleMessage(raw: string): Promise<void> {
    let message: Record<string, unknown>;
    try {
      message = JSON.parse(raw) as Record<string, unknown>;
    } catch {
      this.publish({ status: "error", error: "Computer Use Host received an invalid message" });
      return;
    }
    if (message.type === "computer_use_host_registered" || message.type === "computer_use_host_state_ack") {
      const available = message.available === true;
      this.publish({
        status: !this.enabled ? "disabled" : available ? "ready" : "unavailable",
        error: this.capabilities?.reason ?? null,
      });
      if (message.type === "computer_use_host_registered") this.restorePreviews(message.previews);
      return;
    }
    if (message.type === "computer_use_error") {
      this.publish({ status: "error", error: String(message.error || "Computer Use Host error") });
      return;
    }
    if (message.type === "computer_use_release") {
      const sessionId = typeof message.session_id === "string" ? message.session_id : undefined;
      if (!sessionId) return;
      const agentId = typeof message.agent_id === "string" ? message.agent_id : undefined;
      const keys = message.all_agents === true
        ? this.state.previews
          .filter((preview) => preview.sessionId === sessionId)
          .map((preview) => preview.key)
        : [this.previewKey(sessionId, agentId)];
      const deadline = typeof message.release_deadline_ms === "number"
        ? message.release_deadline_ms
        : Date.now() + COMPUTER_USE_RELEASE_RETENTION_MS;
      for (const key of keys) {
        if (this.state.previews.some((preview) => preview.key === key)) this.scheduleRelease(key, deadline);
      }
      return;
    }
    if (message.type !== "computer_use_request") return;

    const requestId = String(message.request_id || "");
    const action = message.action && typeof message.action === "object"
      ? message.action as Record<string, unknown>
      : {};
    const actionName = String(action.action || "unknown");
    const scope = message.target_scope ?? (message.mode === "foreground_desktop" ? "desktop" : "app_window");
    const mode = scope === "desktop" ? "foreground_desktop" : "background_app";
    const policy = message.delivery_policy ?? "allow_foreground";
    const deliveryPolicy = policy === "strict_background" ? "strict_background" : "allow_foreground";
    const logId = requestId || randomUuid();
    const sessionId = typeof message.session_id === "string" ? message.session_id : undefined;
    const agentId = typeof message.agent_id === "string" ? message.agent_id : undefined;
    const previewKey = this.previewKey(sessionId, agentId);
    const requestToken = requestId || logId;
    this.latestRequestIds.set(previewKey, requestToken);
    this.cancelRelease(previewKey);
    const previousPreview = this.state.previews.find((preview) => preview.key === previewKey);
    const busyPreview: ComputerUsePreview = {
      key: previewKey,
      sessionId,
      agentId,
      mode,
      deliveryPolicy,
      status: "busy",
      action: actionName,
      summary: "Executing…",
      frame: previousPreview?.frame ?? null,
      cursor: previousPreview?.cursor ?? null,
      updatedAt: Date.now(),
    };
    this.publish({
      status: "busy",
      active: true,
      mode,
      deliveryPolicy,
      error: null,
      previews: [...this.state.previews.filter((preview) => preview.key !== previewKey), busyPreview],
      logs: [...this.state.logs, {
        id: logId,
        time: Date.now(),
        action: actionName,
        summary: "Executing…",
        ok: true,
        sessionId,
        agentId,
      }].slice(-100),
    });
    let result: HostResult;
    let invoked = false;
    try {
      if (!this.enabled || !this.capabilities?.gui_available) throw new Error("Computer Use is disabled or a graphical desktop is unavailable");
      if (scope !== "app_window" && scope !== "desktop") throw new Error("Invalid target scope");
      if (policy !== "strict_background" && policy !== "allow_foreground") throw new Error("Invalid delivery policy");
      if (scope === "desktop" && policy === "strict_background") throw new Error("Desktop scope requires allow_foreground");
      if (message.mode !== undefined && message.mode !== mode) throw new Error("Conflicting target scope and mode");
      if (!["observe", "list_windows", "list_displays", "wait"].includes(actionName)
        && this.capabilities.delivery_policy_version !== 1) throw new Error("The host must be upgraded before it can enforce the foreground-delivery policy");
      invoked = true;
      result = await invoke<HostResult>("computer_use_execute", {
        request: { mode, target_scope: scope, delivery_policy: policy, action },
      });
    } catch (error) {
      result = { ok: false, action: actionName, error: error instanceof Error ? error.message : String(error),
        action_dispatched: invoked ? null : false, retry_safe: !invoked, focus_isolation: "unavailable",
        target_scope: scope, delivery_policy: policy };
    }

    const entry: ComputerUseLogEntry = {
      id: logId,
      time: Date.now(),
      action: String(result.action || actionName),
      summary: String(result.summary || result.error || actionName),
      ok: result.ok !== false,
      sessionId,
      agentId,
    };
    const requestIsCurrent = this.latestRequestIds.get(previewKey) === requestToken;
    if (requestIsCurrent) this.latestRequestIds.delete(previewKey);
    const currentPreview = this.state.previews.find((preview) => preview.key === previewKey);
    const shouldPublishCompletion = requestIsCurrent && currentPreview !== undefined;
    const completedPreview: ComputerUsePreview = {
      key: previewKey,
      sessionId,
      agentId,
      mode,
      deliveryPolicy,
      status: result.ok === false ? "error" : "ready",
      action: String(result.action || actionName),
      summary: String(result.summary || result.error || actionName),
      frame: result.screenshot ?? currentPreview?.frame ?? null,
      cursor: result.cursor ?? currentPreview?.cursor ?? null,
      updatedAt: Date.now(),
    };
    const previews = !this.enabled
      ? []
      : shouldPublishCompletion
        ? [...this.state.previews.filter((preview) => preview.key !== previewKey), completedPreview]
        : this.state.previews;
    const anyPreviewIsBusy = previews.some((preview) => preview.status === "busy");
    this.publish({
      status: !this.enabled
        ? "disabled"
        : anyPreviewIsBusy
          ? "busy"
        : shouldPublishCompletion && result.ok === false
          ? "error"
          : this.capabilities?.gui_available ? "ready" : "unavailable",
      active: previews.length > 0,
      previews,
      logs: [...this.state.logs.filter((item) => item.id !== logId), entry].slice(-100),
      error: shouldPublishCompletion && result.ok === false
        ? String(result.error || "Computer Use action failed")
        : this.capabilities?.reason ?? null,
    });
    const releaseDeadline = this.releaseDeadlines.get(previewKey);
    if (releaseDeadline !== undefined && !this.releaseTimers.has(previewKey)) {
      this.scheduleRelease(previewKey, releaseDeadline);
    }
    if (!this.enabled) this.cancelAllReleases();
    this.send({ type: "computer_use_result", request_id: requestId, result });
  }
}

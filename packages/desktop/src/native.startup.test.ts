/* @vitest-environment jsdom */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ensureLocalGateway, installGatewaySuite } from "./native";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

describe("Gateway startup progress bridge", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true });
  });
  afterEach(() => Reflect.deleteProperty(window, "__TAURI_INTERNALS__"));

  it("subscribes before invoking, filters connection/attempt IDs, and cleans up", async () => {
    const unlisten = vi.fn();
    const progress = vi.fn();
    vi.mocked(listen).mockResolvedValue(unlisten);
    vi.mocked(invoke).mockImplementation(async (_command, args) => {
      expect(listen).toHaveBeenCalledWith("gateway-startup-progress", expect.any(Function));
      const { operationId } = args as { operationId: string };
      const callback = vi.mocked(listen).mock.calls[0][1];
      const event = { event: "gateway-startup-progress", id: 1, payload: { connectionId: "local", operationId, stage: "installing", detail: "Downloading dependency" } };
      callback({ ...event, payload: { ...event.payload, connectionId: "other" } });
      callback({ ...event, payload: { ...event.payload, operationId: "old-attempt" } });
      callback(event);
      expect(progress).toHaveBeenCalledTimes(1);
      expect(unlisten).not.toHaveBeenCalled();
      return { ready: true };
    });

    await expect(ensureLocalGateway("local", "http://127.0.0.1:4096", null, null, progress)).resolves.toEqual({ ready: true });
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("removes the listener when installation fails", async () => {
    const unlisten = vi.fn();
    vi.mocked(listen).mockResolvedValue(unlisten);
    vi.mocked(invoke).mockRejectedValue(new Error("pip failed"));
    await expect(ensureLocalGateway("local", "http://127.0.0.1:4096", null, null, vi.fn())).rejects.toThrow("pip failed");
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("bridges suite installation progress and forwards the selected Python", async () => {
    const unlisten = vi.fn();
    const progress = vi.fn();
    vi.mocked(listen).mockResolvedValue(unlisten);
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      expect(command).toBe("install_gateway_suite");
      expect(args).toMatchObject({
        pythonPath: "/opt/python3",
        features: ["search", "debugger", "ripgrep"],
        suite: null,
      });
      const { operationId } = args as { operationId: string };
      const callback = vi.mocked(listen).mock.calls[0][1];
      callback({
        event: "gateway-suite-install-progress",
        id: 1,
        payload: { operationId: "old-attempt", stage: "installing", detail: "stale" },
      });
      callback({
        event: "gateway-suite-install-progress",
        id: 2,
        payload: { operationId, stage: "installing", detail: "Downloading Search" },
      });
      return {
        suite: "search-debugger-ripgrep",
        features: ["search", "debugger", "ripgrep"],
        packageSpec: "crabcode[gateway,search,debugger]==0.1.5 + ripgrep",
        python: "/opt/python3",
      };
    });

    await expect(installGatewaySuite("/opt/python3", ["ripgrep", "debugger", "search", "debugger"], progress)).resolves.toMatchObject({
      suite: "search-debugger-ripgrep",
      features: ["search", "debugger", "ripgrep"],
      python: "/opt/python3",
    });
    expect(progress).toHaveBeenCalledOnce();
    expect(progress).toHaveBeenCalledWith(expect.objectContaining({ detail: "Downloading Search" }));
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("keeps accepting the legacy scalar suite selection", async () => {
    vi.mocked(invoke).mockImplementation(async (_command, args) => {
      expect(args).toMatchObject({
        features: ["search", "debugger"],
        suite: "search-debugger",
      });
      return {
        suite: "search-debugger",
        features: ["search", "debugger"],
        packageSpec: "crabcode[gateway,search,debugger]==0.1.5",
        python: "/opt/python3",
      };
    });

    await expect(installGatewaySuite("/opt/python3", "search-debugger")).resolves.toMatchObject({
      suite: "search-debugger",
    });
  });

  it("does not use desktop events or install anything in browser mode", async () => {
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
    const result = await ensureLocalGateway("local", "http://127.0.0.1:4096", null, null, vi.fn());
    expect(result.ready).toBe(false);
    expect(listen).not.toHaveBeenCalled();
    expect(invoke).not.toHaveBeenCalled();
  });
});

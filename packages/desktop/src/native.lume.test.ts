/* @vitest-environment jsdom */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getLumeInstallStatus, installLume } from "./native";
vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/event", () => ({ listen: vi.fn() }));

describe("Lume native install bridge", () => {
  beforeEach(() => {
    vi.resetAllMocks();
    Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true });
  });
  afterEach(() => Reflect.deleteProperty(window, "__TAURI_INTERNALS__"));

  it("subscribes before invoking and ignores events from other attempts", async () => {
    const unlisten = vi.fn();
    const progress = vi.fn();
    vi.mocked(listen).mockResolvedValue(unlisten);
    vi.mocked(invoke).mockImplementation(async (command, args) => {
      expect(command).toBe("install_lume");
      expect(listen).toHaveBeenCalledWith("lume-install-progress", expect.any(Function));
      const { operationId } = args as { operationId: string };
      const callback = vi.mocked(listen).mock.calls[0][1];
      callback({ event: "lume-install-progress", id: 1, payload: { operationId: "stale", percent: 100 } });
      callback({ event: "lume-install-progress", id: 2, payload: { operationId, percent: 35, stage: "installing", detail: "下载中" } });
      return { available: true, version: "0.5.3" };
    });
    await expect(installLume(progress)).resolves.toMatchObject({ available: true });
    expect(progress).toHaveBeenCalledOnce();
    expect(progress).toHaveBeenCalledWith(expect.objectContaining({ percent: 35 }));
    expect(unlisten).toHaveBeenCalledOnce();
  });

  it("removes the listener after failure and never invokes from browser mode", async () => {
    const unlisten = vi.fn();
    vi.mocked(listen).mockResolvedValue(unlisten);
    vi.mocked(invoke).mockRejectedValue(new Error("download failed"));
    await expect(installLume(vi.fn())).rejects.toThrow("download failed");
    expect(unlisten).toHaveBeenCalledOnce();
    vi.clearAllMocks();
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
    await expect(installLume(vi.fn())).rejects.toThrow("桌面应用");
    await expect(getLumeInstallStatus()).rejects.toThrow("桌面应用");
    expect(invoke).not.toHaveBeenCalled();
    expect(listen).not.toHaveBeenCalled();
  });
});

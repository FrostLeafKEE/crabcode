/* @vitest-environment jsdom */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { StatusBar } from "./StatusBar";
import { updateGatewayStartup } from "./gatewayStartup";
import type { ConnectionPreset, GatewayViewState } from "./types";

const connection = { id: "local", name: "Local", base_url: "http://127.0.0.1:4096" } as ConnectionPreset;
const connecting = { status: "connecting", error: null } as GatewayViewState;

describe("desktop status bar", () => {
  let container: HTMLDivElement;
  let root: Root;
  beforeEach(() => {
    (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-09-16T12:00:00Z"));
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
  });
  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.useRealTimers();
  });

  it("keeps showing elapsed time while waiting for installer output, then stops on success", () => {
    let startup = updateGatewayStartup(undefined, "checking_python", "正在检测 Python 环境");
    act(() => root.render(<StatusBar connection={connection} gateway={connecting} startup={startup} />));
    act(() => vi.advanceTimersByTime(65_000));
    expect(container.querySelector(".status-elapsed")?.textContent).toBe("1 分 5 秒");
    startup = updateGatewayStartup(startup, "installing", "Downloading crabcode");
    act(() => root.render(<StatusBar connection={connection} gateway={connecting} startup={startup} />));
    act(() => container.querySelector<HTMLButtonElement>(".status-current")!.click());
    expect(container.querySelector(".startup-log")?.textContent).toContain("正在检测 Python 环境");
    expect(container.querySelector(".startup-log")?.textContent).toContain("Downloading crabcode");
    startup = updateGatewayStartup(startup, "online", "Gateway 已连接，工作区就绪");
    act(() => root.render(<StatusBar connection={connection} gateway={{ ...connecting, status: "online" }} startup={startup} />));
    expect(container.querySelector('[role="status"]')?.textContent).toBe("就绪");
    expect(container.querySelector(".spin")).toBeNull();
    expect(vi.getTimerCount()).toBe(0);
    act(() => vi.advanceTimersByTime(10_000));
    expect(container.querySelector(".startup-details header")?.textContent).toContain("耗时 1 分 5 秒");
  });

  it("retains the failure and log, exposes retry, and supports Escape", () => {
    const onRetry = vi.fn();
    const startup = updateGatewayStartup(undefined, "error", "Python 3.10 or newer was not found");
    act(() => root.render(<StatusBar connection={connection} gateway={{ ...connecting, status: "error", error: startup.detail }} startup={startup} onRetry={onRetry} />));
    expect(container.querySelector('[role="status"]')?.textContent).toContain("Python 3.10");
    act(() => container.querySelector<HTMLButtonElement>(".status-retry")!.click());
    expect(onRetry).toHaveBeenCalledOnce();
    act(() => container.querySelector<HTMLButtonElement>(".status-current")!.click());
    expect(container.querySelector(".startup-log")?.textContent).toContain(startup.detail);
    act(() => container.querySelector("footer")!.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
    expect(container.querySelector(".startup-details")).toBeNull();
    expect(document.activeElement).toBe(container.querySelector(".status-current"));
  });

  it("shows configuration loading and a readable configuration error", () => {
    act(() => root.render(<StatusBar loading />));
    expect(container.querySelector('[role="status"]')?.textContent).toBe("正在读取桌面配置…");
    act(() => root.render(<StatusBar error="配置文件不可读" />));
    expect(container.querySelector('[role="status"]')?.textContent).toBe("配置文件不可读");
    expect(container.querySelector(".spin")).toBeNull();
  });

  it("bounds log history and starts a clean history on retry", () => {
    let startup = updateGatewayStartup(undefined, "installing", "first", 0);
    for (let i = 1; i <= 150; i++) startup = updateGatewayStartup(startup, "installing", `line ${i}`, i);
    expect(startup.history).toHaveLength(100);
    expect(startup.history[0].detail).toBe("line 51");
    expect(updateGatewayStartup(startup, "installing", "line 150").history).toHaveLength(100);
    expect(updateGatewayStartup(undefined, "connecting", "retry", 200).startedAt).toBe(200);
  });
});

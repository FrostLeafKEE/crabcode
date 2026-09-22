/* @vitest-environment jsdom */
import { act } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { ModelSettingsPanel } from "./ModelSettingsPanel";
import type { GatewayViewState, ModelSettingsResponse } from "./types";

it("tests the selected model, blocks duplicate clicks and discards stale results", async () => {
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
  const container = document.createElement("div");
  document.body.append(container);
  const root = createRoot(container);
  const data: ModelSettingsResponse = {
    cwd: "D:/project", default_model: "A", groups: {}, sources: [], warnings: [],
    models: ["A", "B"].map(name => ({ name, group: null, is_default: name === "A", configured: {}, effective: {}, overridden_fields: [], sources: [] })),
  };
  let finish!: (result: { ok: boolean; message: string }) => void;
  const onTest = vi.fn(() => new Promise<{ ok: boolean; message: string }>(resolve => { finish = resolve; }));
  const button = () => Array.from(container.querySelectorAll("button")).find(b => /测试模型|测试中/.test(b.textContent || ""))!;
  try {
    await act(async () => root.render(<ModelSettingsPanel activeConnection={null} activeProject={null} gateway={{ status: "online" } as GatewayViewState} data={data} loading={false} error={null} onRefresh={() => {}} onTest={onTest} />));
    await act(async () => button().click());
    expect(onTest).toHaveBeenCalledWith("A");
    expect(button().disabled).toBe(true);
    await act(async () => container.querySelector<HTMLButtonElement>('[aria-label="查看模型 B"]')!.click());
    await act(async () => finish({ ok: true, message: "old A result" }));
    expect(container.textContent).not.toContain("old A result");
    await act(async () => button().click());
    expect(onTest).toHaveBeenLastCalledWith("B");
    await act(async () => finish({ ok: false, message: "HTTP 401：认证失败" }));
    expect(container.querySelector('[role="status"]')?.textContent).toContain("HTTP 401");
    expect(container.querySelector('[role="status"]')?.classList.contains("model-test-success")).toBe(false);
    await act(async () => button().click());
    await act(async () => finish({ ok: true, message: "连接成功" }));
    expect(container.querySelector('[role="status"]')?.textContent).toContain("连接成功");
    expect(container.querySelector('[role="status"]')?.classList.contains("model-test-success")).toBe(true);
    expect(container.textContent).not.toContain("HTTP 401");
  } finally {
    await act(async () => root.unmount());
    container.remove();
  }
});

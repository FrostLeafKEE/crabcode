/* @vitest-environment jsdom */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { GatewayApi } from "./gateway";
import type { ConnectionPreset, ProjectPreset, UsageDailyResponse } from "./types";
import { UsageSettingsPanel } from "./UsageSettingsPanel";

const connection = (id: string): ConnectionPreset => ({
  id, name: id, base_url: `http://127.0.0.1:${id === "a" ? 4096 : 4097}`,
  credential_ref: null, allow_insecure_remote: false, document_workspace_root: null,
  projects: [], last_project_path: null, last_project_id: null,
});
const project: ProjectPreset = { id: "p", kind: "project", path: "C:\\work\\p",
  name: "p", directories: [], last_session_id: null };

function response(start: string, end: string, total: number): UsageDailyResponse {
  return { start, end, timezone: "UTC", scope: "global", generated_at: new Date().toISOString(),
    tracking_started_at: new Date().toISOString(),
    first_recorded_at: new Date().toISOString(), last_recorded_at: new Date().toISOString(),
    days: [{ date: start, input_tokens: total, output_tokens: 0, total_tokens: total,
             request_count: 1, unknown_requests: 0, missing_requests: 0, partial_requests: 0, coverage: "complete" }],
    models: [{ key: "openai/m", provider: "openai", model_id: "m", model: "openai/m", total_tokens: total,
      recorded_request_count: 1, points: [{ date: start, total_tokens: total }] }],
    summary: { input_tokens: total, output_tokens: 0, total_tokens: total,
      request_count: 1, unknown_requests: 0, missing_requests: 0, partial_requests: 0 } };
}

function changeDate(input: HTMLInputElement, value: string) {
  const setter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
  setter.call(input, value);
  input.dispatchEvent(new Event("input", { bubbles: true }));
  input.dispatchEvent(new Event("change", { bubbles: true }));
}

describe("UsageSettingsPanel", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement("div");
    document.body.append(container);
    root = createRoot(container);
    localStorage.clear();
  });
  afterEach(() => {
    act(() => root.unmount());
    container.remove();
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("queries month and week independently, only after the date query button", async () => {
    const usage = vi.spyOn(GatewayApi.prototype, "usageDaily")
      .mockImplementation(async (start, end) => response(start, end, 20));
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    expect(usage).toHaveBeenCalledTimes(2);
    const heatStart = container.querySelector<HTMLInputElement>('input[aria-label="热力图开始日期"]')!;
    const end = container.querySelector<HTMLInputElement>('input[aria-label="热力图结束日期"]')!;
    const query = container.querySelectorAll<HTMLButtonElement>(".usage-actions button.primary")[0];
    expect(query.textContent?.trim()).toBe("查询所选日期");
    expect(query.disabled).toBe(true);
    await act(async () => query.click());
    expect(usage).toHaveBeenCalledTimes(2);
    await act(async () => changeDate(heatStart, end.value));
    expect(usage).toHaveBeenCalledTimes(2);
    expect(query.disabled).toBe(false);
    await act(async () => query.click());
    expect(usage).toHaveBeenCalledTimes(3);
    expect(usage.mock.calls[2][0]).toBe(end.value);
    expect(query.disabled).toBe(true);
    expect(container.textContent).toContain("Token 使用热力图");
  });

  it("keeps the trend date query disabled until its dates change", async () => {
    const usage = vi.spyOn(GatewayApi.prototype, "usageDaily")
      .mockImplementation(async (start, end) => response(start, end, 20));
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    const lineStart = container.querySelector<HTMLInputElement>('input[aria-label="折线图开始日期"]')!;
    const lineEnd = container.querySelector<HTMLInputElement>('input[aria-label="折线图结束日期"]')!;
    const query = container.querySelectorAll<HTMLButtonElement>(".usage-actions button.primary")[1];
    expect(query.disabled).toBe(true);
    await act(async () => changeDate(lineStart, lineEnd.value));
    expect(query.disabled).toBe(false);
    await act(async () => query.click());
    expect(usage).toHaveBeenCalledTimes(3);
    expect(usage.mock.calls[2][0]).toBe(lineEnd.value);
    expect(query.disabled).toBe(true);
  });

  it("applies preset ranges immediately and marks the active preset", async () => {
    const usage = vi.spyOn(GatewayApi.prototype, "usageDaily")
      .mockImplementation(async (start, end) => response(start, end, 20));
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    expect(usage).toHaveBeenCalledTimes(2);
    const [heatActions, lineActions] = container.querySelectorAll(".usage-actions");
    const button = (actions: Element, label: string) => [...actions.querySelectorAll("button")]
      .find((item) => item.textContent === label)!;

    const heatThirty = button(heatActions, "最近 30 天");
    await act(async () => heatThirty.click());
    expect(usage).toHaveBeenCalledTimes(3);
    expect(usage.mock.calls[2][0]).toBe(container.querySelector<HTMLInputElement>('input[aria-label="热力图开始日期"]')!.value);
    expect(heatThirty.getAttribute("aria-pressed")).toBe("true");

    const lineMonth = button(lineActions, "本月");
    await act(async () => lineMonth.click());
    expect(usage).toHaveBeenCalledTimes(4);
    expect(usage.mock.calls[3][0]).toBe(container.querySelector<HTMLInputElement>('input[aria-label="折线图开始日期"]')!.value);
    expect(lineMonth.getAttribute("aria-pressed")).toBe("true");

    await act(async () => lineMonth.click());
    expect(usage).toHaveBeenCalledTimes(5);
  });

  it("ignores a stale Gateway response after switching connections", async () => {
    let release!: (value: UsageDailyResponse) => void;
    const stale = new Promise<UsageDailyResponse>((resolve) => { release = resolve; });
    const usage = vi.spyOn(GatewayApi.prototype, "usageDaily")
      .mockImplementation(function (this: GatewayApi, start, end) {
        return this.connection.id === "a" ? stale : Promise.resolve(response(start, end, 222));
      });
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    await act(async () => root.render(<UsageSettingsPanel connection={connection("b")} project={project} online />));
    expect(usage).toHaveBeenCalledTimes(4);
    expect(container.textContent).toContain("222");
    await act(async () => release(response("2026-09-01", "2026-09-02", 999)));
    expect(container.textContent).not.toContain("999");
  });

  it("does not request usage while disconnected", async () => {
    const usage = vi.spyOn(GatewayApi.prototype, "usageDaily");
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online={false} />));
    expect(usage).not.toHaveBeenCalled();
    expect(container.textContent).toContain("连接 Gateway 后才能查看使用情况");
  });

  it("groups models beyond the leading seven without double counting", async () => {
    vi.spyOn(GatewayApi.prototype, "usageDaily").mockImplementation(async (start, end) => {
      const data = response(start, end, 9);
      data.models = Array.from({ length: 9 }, (_, index) => ({
        key: `openai/m${index}`, provider: "openai", model_id: `m${index}`,
        model: `openai/m${index}`, total_tokens: 1,
        recorded_request_count: 1, points: [{ date: start, total_tokens: 1 }],
      }));
      return data;
    });
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    const legend = container.querySelectorAll(".usage-model-legend button");
    expect(legend).toHaveLength(8);
    expect([...legend].find((button) => button.textContent?.includes("其他模型"))?.textContent).toContain("2");
  });

  it("shows a themed point tooltip without native SVG titles or focus rectangles", async () => {
    vi.spyOn(GatewayApi.prototype, "usageDaily")
      .mockImplementation(async (start, end) => response(start, end, 1234));
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    const chart = container.querySelector(".usage-trend-chart")!;
    expect(chart.querySelectorAll("title, rect")).toHaveLength(0);
    const point = chart.querySelector<SVGCircleElement>(".usage-trend-hit")!;
    expect(point.getAttribute("aria-label")).toContain("1,234 Token");

    await act(async () => point.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })));
    expect(chart.querySelector('[role="tooltip"]')?.textContent).toContain("1,234");
    expect(chart.querySelector('[role="tooltip"]')?.textContent).toContain("openai/m");
    await act(async () => point.dispatchEvent(new MouseEvent("mouseout", { bubbles: true })));
    expect(chart.querySelector('[role="tooltip"]')).toBeNull();
  });

  it("combines equal-value points into one hit target listing every model", async () => {
    vi.spyOn(GatewayApi.prototype, "usageDaily").mockImplementation(async (start, end) => {
      const data = response(start, end, 0);
      data.models.push({ key: "codex/other", provider: "codex", model_id: "other",
        model: "codex/other", total_tokens: 0, recorded_request_count: 1,
        points: [{ date: start, total_tokens: 0 }] });
      return data;
    });
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    const chart = container.querySelector(".usage-trend-chart")!;
    expect(chart.querySelectorAll(".usage-trend-hit")).toHaveLength(1);
    expect(chart.querySelector(".usage-overlap-count")?.textContent).toBe("2");
    const point = chart.querySelector<SVGCircleElement>(".usage-trend-hit")!;
    const originalCoordinates = [point.getAttribute("cx"), point.getAttribute("cy")];
    expect(point.getAttribute("aria-label")).toContain("openai/m");
    expect(point.getAttribute("aria-label")).toContain("codex/other");
    await act(async () => point.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })));
    const tooltip = chart.querySelector('[role="tooltip"]')!;
    expect(tooltip.textContent).toContain("2 个模型");
    expect(tooltip.querySelectorAll(".usage-trend-tooltip-item")).toHaveLength(2);
    expect(tooltip.textContent).toContain("codex/other");
    expect(tooltip.textContent).toContain("openai/m");

    const firstChip = container.querySelector<HTMLButtonElement>(".usage-model-legend-item > button")!;
    await act(async () => firstChip.click());
    expect(chart.querySelector(".usage-overlap-count")).toBeNull();
    const remaining = chart.querySelector<SVGCircleElement>(".usage-trend-hit")!;
    expect([remaining.getAttribute("cx"), remaining.getAttribute("cy")]).toEqual(originalCoordinates);
    await act(async () => remaining.dispatchEvent(new MouseEvent("mouseover", { bubbles: true })));
    expect(chart.querySelectorAll(".usage-trend-tooltip-item")).toHaveLength(1);
  });

  it("right-click removes only the chart series and keeps history and totals", async () => {
    vi.spyOn(GatewayApi.prototype, "usageDaily")
      .mockImplementation(async (start, end) => response(start, end, 1234));
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    const chip = container.querySelector<HTMLButtonElement>(".usage-model-legend-item > button")!;
    await act(async () => chip.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true })));
    expect(container.querySelector('[role="menu"]')?.textContent).toContain("不删除历史用量");
    await act(async () => container.querySelector<HTMLButtonElement>('[role="menuitem"]')!.click());
    expect(container.querySelectorAll(".usage-trend-chart circle")).toHaveLength(0);
    expect(container.querySelectorAll(".usage-model-legend-item")).toHaveLength(0);
    expect(container.textContent).toContain("当前范围的模型均已从趋势图移除");
    expect(container.textContent).toContain("1,234已报告 Token");
    expect(container.querySelectorAll(".usage-details table")[1]?.textContent).toContain("openai/m");
    expect(localStorage.length).toBe(1);

    const restorePanel = container.querySelector<HTMLDetailsElement>(".usage-restore-models")!;
    expect(restorePanel.open).toBe(false);
    await act(async () => restorePanel.querySelector("summary")!.click());
    expect(restorePanel.open).toBe(true);
    await act(async () => restorePanel.querySelector<HTMLButtonElement>('[aria-label="恢复模型 openai/m"]')!.click());
    expect(container.querySelectorAll(".usage-model-legend-item")).toHaveLength(1);
    expect(localStorage.length).toBe(0);
  });

  it("restores selected models individually and keeps restore-all inside the expanded panel", async () => {
    vi.spyOn(GatewayApi.prototype, "usageDaily").mockImplementation(async (start, end) => {
      const data = response(start, end, 20);
      data.models.push({ key: "codex/other", provider: "codex", model_id: "other", model: "codex/other",
        total_tokens: 5, recorded_request_count: 1, points: [{ date: start, total_tokens: 5 }] });
      return data;
    });
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    const dismissFirst = async () => {
      const chip = container.querySelector<HTMLButtonElement>(".usage-model-legend-item > button")!;
      await act(async () => chip.dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true })));
      await act(async () => container.querySelector<HTMLButtonElement>('[role="menuitem"]')!.click());
    };
    await dismissFirst();
    await dismissFirst();
    const panel = container.querySelector<HTMLDetailsElement>(".usage-restore-models")!;
    expect(panel.querySelectorAll("li")).toHaveLength(2);
    expect(panel.querySelector<HTMLButtonElement>(".usage-restore-all")).not.toBeNull();
    await act(async () => panel.querySelector("summary")!.click());
    await act(async () => panel.querySelector<HTMLButtonElement>('[aria-label="恢复模型 codex/other"]')!.click());
    expect(panel.querySelectorAll("li")).toHaveLength(1);
    expect(panel.querySelector("summary")?.textContent).toContain("（1）");
    expect(container.querySelectorAll(".usage-model-legend-item")).toHaveLength(1);
    expect(localStorage.length).toBe(1);
    await act(async () => panel.querySelector<HTMLButtonElement>(".usage-restore-all")!.click());
    expect(container.querySelector(".usage-restore-models")).toBeNull();
    expect(container.querySelectorAll(".usage-model-legend-item")).toHaveLength(2);
    expect(localStorage.length).toBe(0);
  });

  it("keeps the restore list available when the selected range has no model records", async () => {
    let emptyRange = false;
    vi.spyOn(GatewayApi.prototype, "usageDaily").mockImplementation(async (start, end) => {
      const data = response(start, end, 0);
      if (emptyRange) data.models = [];
      return data;
    });
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    await act(async () => container.querySelector<HTMLButtonElement>(".usage-model-legend-item > button")!
      .dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true })));
    await act(async () => container.querySelector<HTMLButtonElement>('[role="menuitem"]')!.click());
    emptyRange = true;
    await act(async () => container.querySelector<HTMLButtonElement>(".usage-scope button")!.click());
    const panel = container.querySelector<HTMLDetailsElement>(".usage-restore-models")!;
    expect(panel.querySelector("summary")?.textContent).toContain("（1）");
    await act(async () => panel.querySelector("summary")!.click());
    expect(panel.querySelector<HTMLButtonElement>('[aria-label="恢复模型 openai/m"]')).not.toBeNull();
  });

  it("keeps a removed model hidden across date changes, then restores it on a new zero-token request", async () => {
    let requestCount = 1;
    vi.spyOn(GatewayApi.prototype, "usageDaily").mockImplementation(async (start, end) => {
      const data = response(start, end, 0);
      data.models[0].recorded_request_count = requestCount;
      return data;
    });
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    await act(async () => container.querySelector<HTMLButtonElement>(".usage-model-legend-item > button")!
      .dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true })));
    await act(async () => container.querySelector<HTMLButtonElement>('[role="menuitem"]')!.click());
    const lineActions = container.querySelectorAll(".usage-actions")[1];
    const month = [...lineActions.querySelectorAll<HTMLButtonElement>("button")].find((button) => button.textContent === "本月")!;
    await act(async () => month.click());
    expect(container.querySelectorAll(".usage-model-legend-item")).toHaveLength(0);
    const refresh = [...container.querySelectorAll<HTMLButtonElement>(".usage-scope button")].find((button) => button.textContent === "刷新")!;
    await act(async () => refresh.click());
    expect(container.querySelectorAll(".usage-model-legend-item")).toHaveLength(0);
    requestCount++;
    await act(async () => refresh.click());
    expect(container.querySelectorAll(".usage-model-legend-item")).toHaveLength(1);
    expect(localStorage.length).toBe(0);
  });

  it("checks for new model requests automatically while the page is open", async () => {
    vi.useFakeTimers();
    let requestCount = 1;
    const usage = vi.spyOn(GatewayApi.prototype, "usageDaily").mockImplementation(async (start, end) => {
      const data = response(start, end, 0);
      data.models[0].recorded_request_count = requestCount;
      return data;
    });
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    await act(async () => container.querySelector<HTMLButtonElement>(".usage-model-legend-item > button")!
      .dispatchEvent(new MouseEvent("contextmenu", { bubbles: true, cancelable: true })));
    await act(async () => container.querySelector<HTMLButtonElement>('[role="menuitem"]')!.click());
    requestCount++;
    await act(async () => vi.advanceTimersByTimeAsync(30_000));
    expect(usage).toHaveBeenCalledTimes(4);
    expect(container.querySelectorAll(".usage-model-legend-item")).toHaveLength(1);
  });

  it("shows unrecorded history as unavailable instead of zero", async () => {
    vi.spyOn(GatewayApi.prototype, "usageDaily").mockImplementation(async (start, end) => {
      const data = response(start, end, 0);
      data.tracking_started_at = null;
      data.days[0].coverage = "unavailable";
      data.days[0].total_tokens = null;
      data.models = [];
      return data;
    });
    await act(async () => root.render(<UsageSettingsPanel connection={connection("a")} project={project} online />));
    expect(container.querySelector(".usage-total")?.textContent).toContain("—");
    expect(container.textContent).toContain("尚未开始记录使用情况");
  });
});

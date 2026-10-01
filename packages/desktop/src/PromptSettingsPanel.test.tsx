/* @vitest-environment jsdom */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PromptSettingsPanel } from "./PromptSettingsPanel";
import type { GatewayViewState, PromptSettingsMutation, PromptSettingsResponse } from "./types";

const gateway: GatewayViewState = {
  status: "online",
  error: null,
  token: null,
  tokenExpiresAt: 0,
  workspace: null,
  sessionsByProject: {},
  models: [],
  runningCount: 0,
  pendingCount: 0,
};

const data: PromptSettingsResponse = {
  cwd: "/work/crabcode",
  active_template_id: null,
  templates: [{
    id: "care",
    name: "客服",
    sections: { intro: "custom identity" },
    source: "userSettings",
  }],
  user_prompts: [{
    id: "p1",
    text: "用中文回答",
    enabled: false,
    source: "userSettings",
  }],
  sections: [
    { key: "intro", label: "介绍" },
    { key: "extra", label: "额外段落" },
  ],
  warnings: [],
  editable_sources: [{
    id: "userSettings",
    label: "用户配置",
    path: "/home/settings.json",
    exists: true,
    writable: true,
  }],
};

function setValue(element: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement, value: string) {
  const prototype = Object.getPrototypeOf(element) as object;
  const setter = Object.getOwnPropertyDescriptor(prototype, "value")?.set
    ?? Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")?.set;
  setter?.call(element, value);
  element.dispatchEvent(new Event("input", { bubbles: true }));
  element.dispatchEvent(new Event("change", { bubbles: true }));
}

describe("PromptSettingsPanel", () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(() => {
    act(() => root.unmount());
    container.remove();
  });

  it("starts on the default template and saves a named template", async () => {
    const onMutate = vi.fn(async (_mutation: PromptSettingsMutation) => undefined);
    await act(async () => {
      root.render(
        <PromptSettingsPanel
          activeConnection={null}
          activeProject={null}
          gateway={gateway}
          data={data}
          loading={false}
          error={null}
          onRefresh={() => undefined}
          onMutate={onMutate}
        />,
      );
    });

    const options = Array.from(container.querySelectorAll<HTMLButtonElement>('[aria-label="使用的提示词模版"] [role="option"]'));
    expect(options[0]?.textContent).toContain("默认");
    expect(options[0]?.getAttribute("aria-selected")).toBe("true");
    expect(options.map((option) => option.textContent ?? "")).toEqual(expect.arrayContaining([expect.stringContaining("客服")]));
    expect(container.querySelector<HTMLTextAreaElement>('[aria-label="介绍"]')?.placeholder).toBe("留空则使用默认");

    const name = container.querySelector<HTMLInputElement>('[aria-label="模版名称"]')!;
    const intro = container.querySelector<HTMLTextAreaElement>('[aria-label="介绍"]')!;
    await act(async () => {
      setValue(name, "客服副本");
      setValue(intro, "先给出结论");
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>(".prompt-settings-actions .primary")!.click();
    });

    expect(onMutate).toHaveBeenCalledWith(expect.objectContaining({
      action: "save_template",
      source: "userSettings",
      template_name: "客服副本",
      sections: expect.objectContaining({ intro: "先给出结论" }),
    }));
    expect(onMutate.mock.calls.map((call) => call[0].template_id)).toEqual([undefined]);
  });

  it("checks a saved user prompt to append it", async () => {
    const onMutate = vi.fn(async (_mutation: PromptSettingsMutation) => undefined);
    await act(async () => {
      root.render(
        <PromptSettingsPanel
          activeConnection={null}
          activeProject={null}
          gateway={gateway}
          data={data}
          loading={false}
          error={null}
          onRefresh={() => undefined}
          onMutate={onMutate}
        />,
      );
    });

    const checkbox = container.querySelector<HTMLInputElement>('[aria-label="追加提示：用中文回答"]')!;
    expect(checkbox.checked).toBe(false);
    await act(async () => checkbox.click());

    expect(onMutate).toHaveBeenCalledWith(expect.objectContaining({
      action: "set_user_prompt_enabled",
      source: "userSettings",
      prompt_id: "p1",
      enabled: true,
    }));
  });

  it("deletes a template and a user prompt from the list", async () => {
    const onMutate = vi.fn(async (_mutation: PromptSettingsMutation) => undefined);
    await act(async () => {
      root.render(
        <PromptSettingsPanel
          activeConnection={null}
          activeProject={null}
          gateway={gateway}
          data={data}
          loading={false}
          error={null}
          onRefresh={() => undefined}
          onMutate={onMutate}
        />,
      );
    });

    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="删除模版 客服"]')!.click();
    });
    await act(async () => {
      container.querySelector<HTMLButtonElement>('[aria-label="移除提示：用中文回答"]')!.click();
    });

    expect(onMutate).toHaveBeenNthCalledWith(1, expect.objectContaining({
      action: "delete_template",
      source: "userSettings",
      template_id: "care",
    }));
    expect(onMutate).toHaveBeenNthCalledWith(2, expect.objectContaining({
      action: "delete_user_prompt",
      source: "userSettings",
      prompt_id: "p1",
    }));
  });

  it("filters templates from the search box", async () => {
    const onMutate = vi.fn(async (_mutation: PromptSettingsMutation) => undefined);
    await act(async () => {
      root.render(
        <PromptSettingsPanel
          activeConnection={null}
          activeProject={null}
          gateway={gateway}
          data={data}
          loading={false}
          error={null}
          onRefresh={() => undefined}
          onMutate={onMutate}
        />,
      );
    });

    const search = container.querySelector<HTMLInputElement>('[aria-label="搜索提示词模版"]')!;
    await act(async () => setValue(search, "客服"));
    const options = Array.from(container.querySelectorAll('[aria-label="使用的提示词模版"] [role="option"]'));
    expect(options.map((option) => option.textContent ?? "")).toEqual([expect.stringContaining("客服")]);
  });
});

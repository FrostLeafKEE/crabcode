/* @vitest-environment jsdom */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { VirtualMachineSettings } from "./VirtualMachineSettings";
import { RuntimeSettingsPanel } from "./RuntimeSettingsPanel";
import { DEFAULT_VM_CONFIG, vmHostId } from "./virtualMachine";
import type { DesktopSettings, RuntimeSettingsResponse } from "./types";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("./native", () => ({ isDesktopShell: () => true }));
let container: HTMLDivElement;
let root: Root;
beforeEach(() => {
  vi.stubGlobal("IS_REACT_ACT_ENVIRONMENT", true);
  invokeMock.mockReset(); container = document.createElement("div"); document.body.appendChild(container); root = createRoot(container);
});
afterEach(() => { act(() => root.unmount()); container.remove(); });
const settings = { computer_use_environment: "local_vm", computer_use_vm: DEFAULT_VM_CONFIG } as DesktopSettings;
function button(text: string) { return [...container.querySelectorAll("button")].find(b => b.textContent === text)!; }

describe("local VM settings", () => {
  it("keeps host policy controls out of the isolated VM workflow", () => {
    act(() => root.render(<RuntimeSettingsPanel localVmSelected
        computerUseEnvironment={<VirtualMachineSettings settings={settings} onChange={vi.fn()} />}
        activeConnection={null} activeProject={null} gateway={{ status: "online" } as never}
        data={{ warnings: [], sources: [], extra_tools: [], extra_tools_by_source: {}, snapshot_enabled: true, snapshot_max_size_mb: 1024 } as unknown as RuntimeSettingsResponse}
        loading={false} error={null} onRefresh={vi.fn()} />));
    expect(container.querySelector('[aria-label="Computer Use 前台权限"]')).toBeNull();
    expect(container.querySelector('[aria-label="Computer Use 操作目标"]')).toBeNull();
    expect(container.textContent).toContain("独立的 macOS 桌面");
    expect(container.querySelectorAll('input[type="password"]')).toHaveLength(1);
  });
  it("does not persist the setup password and can pause during an active task", async () => {
    const onChange = vi.fn(); invokeMock.mockResolvedValue({ ok: true });
    act(() => root.render(<VirtualMachineSettings settings={settings} taskBusy onChange={onChange} />));
    expect(button("本机").disabled).toBe(true);
    expect(button("暂停自动操作").disabled).toBe(false);
    await act(async () => button("暂停自动操作").click());
    expect(invokeMock).toHaveBeenCalledWith("computer_use_vm_manage", { config: DEFAULT_VM_CONFIG, operation: "takeover", password: null, create: null });
    expect(onChange).not.toHaveBeenCalled();
  });
  it("keeps distinct routes for different VM configurations", () => {
    const first = vmHostId("desktop-one", DEFAULT_VM_CONFIG);
    expect(vmHostId("desktop-one", { ...DEFAULT_VM_CONFIG })).toBe(first);
    expect(vmHostId("desktop-one", { ...DEFAULT_VM_CONFIG, name: "another" })).not.toBe(first);
    expect(vmHostId("desktop-one", { ...DEFAULT_VM_CONFIG, storage: "external" })).not.toBe(first);
    expect(vmHostId("desktop-one", { ...DEFAULT_VM_CONFIG, user: "another" })).not.toBe(first);
  });
});

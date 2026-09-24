import { beforeEach, describe, expect, it, vi } from "vitest";
import { register, unregister, type ShortcutHandler } from "@tauri-apps/plugin-global-shortcut";
import { approvalShortcutFromKey, approvalShortcutKeyLabels, bindApprovalShortcuts, DEFAULT_APPROVAL_SHORTCUTS, normalizeApprovalShortcut, uniquePendingApproval } from "./approvalShortcuts";
import type { SessionViewState } from "./types";

vi.mock("@tauri-apps/plugin-global-shortcut", () => ({ register: vi.fn(), unregister: vi.fn() }));

function session(id: string, toolUseIds: string[], connected = true): SessionViewState {
  return {
    id, cwd: "C:\\work", title: id, loading: false, busy: true,
    connected, operationId: null, status: null, error: null,
    items: toolUseIds.map((tool_use_id) => ({
      id: tool_use_id, kind: "permission", status: "pending", tool_use_id,
    })),
  };
}

describe("approval shortcut target", () => {
  it("records physical keys and requires a modifier to avoid approving while typing", () => {
    expect(approvalShortcutFromKey({ code: "KeyY", ctrlKey: true, altKey: true, shiftKey: false, metaKey: false }))
      .toBe("Ctrl+Alt+Y");
    expect(approvalShortcutFromKey({ code: "Digit1", ctrlKey: true, altKey: false, shiftKey: true, metaKey: false }))
      .toBe("Ctrl+Shift+1");
    expect(approvalShortcutFromKey({ code: "KeyY", ctrlKey: false, altKey: true, shiftKey: false, metaKey: true }))
      .toBe("Alt+Super+Y");
    for (const invalid of ["Y", "Shift+Y", "Ctrl+Ctrl+Y", "Win+Y", "Ctrl+Enter", "Ctrl+F25", "Ctrl+ControlLeft"]) {
      expect(normalizeApprovalShortcut(invalid)).toBeNull();
    }
  });
  it("shows platform key names without changing the registered physical keys", () => {
    expect(approvalShortcutKeyLabels("Ctrl+Alt+Shift+F9", "MacIntel")).toEqual(["⌃", "⌥", "⇧", "F9"]);
    expect(approvalShortcutKeyLabels("Alt+Super+Y", "MacIntel")).toEqual(["⌥", "⌘", "Y"]);
    expect(approvalShortcutKeyLabels("Ctrl+Alt+Shift+F9", "Win32")).toEqual(["Ctrl", "Alt", "Shift", "F9"]);
    expect(approvalShortcutKeyLabels("Alt+Super+Y", "Win32")).toEqual(["Alt", "Win", "Y"]);
  });
  it("selects only one connected pending permission with its exact session and tool ID", () => {
    const target = uniquePendingApproval({
      stale: session("stale", ["old"], false),
      live: session("live", ["new"]),
    });
    expect(target?.sessionKey).toBe("live");
    expect(target?.item.tool_use_id).toBe("new");
  });

  it("does not select a request when multiple permissions are pending", () => {
    expect(uniquePendingApproval({ first: session("first", ["one"]), second: session("second", ["two"]) })).toBeNull();
    expect(uniquePendingApproval({ first: session("first", ["one", "two"]) })).toBeNull();
  });

  it("does not select a request with no tool ID", () => {
    const current = session("live", [""]);
    expect(uniquePendingApproval({ live: current })).toBeNull();
  });
});

describe("approval shortcut registration", () => {
  const handlers = new Map<string, ShortcutHandler>();
  beforeEach(() => {
    vi.resetAllMocks();
    handlers.clear();
    vi.mocked(register).mockImplementation(async (shortcut, handler) => { handlers.set(String(shortcut), handler); });
    vi.mocked(unregister).mockResolvedValue(undefined);
  });

  it("uses configured keys for one-time approval, denial and session approval", async () => {
    const onDecision = vi.fn();
    const binding = bindApprovalShortcuts({
      enabled: true, approve: "Ctrl+Alt+Y", deny: "Ctrl+Alt+N", always_allow: "Alt+Super+A",
    }, onDecision, vi.fn());
    await binding.ready;
    expect(register).toHaveBeenCalledWith("Alt+Super+A", expect.any(Function));
    for (const [shortcut, handler] of handlers) {
      handler({ shortcut, id: 1, state: "Released" });
      handler({ shortcut, id: 1, state: "Pressed" });
    }
    expect(onDecision.mock.calls).toEqual([[true, false], [false, false], [true, true]]);
    await binding.dispose();
    for (const [shortcut, handler] of handlers) handler({ shortcut, id: 1, state: "Pressed" });
    expect(onDecision).toHaveBeenCalledTimes(3);
  });

  it("finishes old cleanup before registering changed keys and leaves disabled settings unregistered", async () => {
    const first = bindApprovalShortcuts(DEFAULT_APPROVAL_SHORTCUTS, vi.fn(), vi.fn());
    await first.ready;
    let finishUnregister!: () => void;
    vi.mocked(unregister).mockImplementationOnce(() => new Promise<void>((resolve) => { finishUnregister = resolve; }));
    const disposing = first.dispose();
    const second = bindApprovalShortcuts({ ...DEFAULT_APPROVAL_SHORTCUTS, approve: "Ctrl+Alt+Y" }, vi.fn(), vi.fn());
    await vi.waitFor(() => expect(unregister).toHaveBeenCalled());
    expect(register).toHaveBeenCalledTimes(3);
    finishUnregister();
    await disposing;
    await second.ready;
    expect(register).toHaveBeenCalledTimes(6);
    expect(register).toHaveBeenNthCalledWith(4, "Ctrl+Alt+Y", expect.any(Function));
    await second.dispose();
    const disabled = bindApprovalShortcuts({ ...DEFAULT_APPROVAL_SHORTCUTS, enabled: false }, vi.fn(), vi.fn());
    await disabled.ready;
    expect(register).toHaveBeenCalledTimes(6);
    await disabled.dispose();
  });

  it("rolls back a partial registration conflict without accepting a decision", async () => {
    const onDecision = vi.fn();
    const onError = vi.fn();
    vi.mocked(register).mockImplementationOnce(async (shortcut, handler) => {
      handlers.set(String(shortcut), handler);
      handler({ shortcut: String(shortcut), id: 1, state: "Pressed" });
    }).mockRejectedValueOnce(new Error("already registered"));
    const binding = bindApprovalShortcuts(DEFAULT_APPROVAL_SHORTCUTS, onDecision, onError);
    await binding.ready;
    expect(unregister).toHaveBeenCalledWith(DEFAULT_APPROVAL_SHORTCUTS.approve);
    expect(onError).toHaveBeenCalledWith(expect.stringContaining("already registered"));
    expect(onDecision).not.toHaveBeenCalled();
    await binding.dispose();
  });
});

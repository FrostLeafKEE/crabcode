import { describe, expect, it } from "vitest";
import { uniquePendingApproval } from "./approvalShortcuts";
import type { SessionViewState } from "./types";

function session(id: string, toolUseIds: string[], connected = true): SessionViewState {
  return {
    id, cwd: "C:\\work", title: id, loading: false, busy: true,
    connected, operationId: null, status: null, error: null,
    items: toolUseIds.map((tool_use_id) => ({
      id: tool_use_id, kind: "permission", status: "pending", tool_use_id,
    })),
  };
}

describe("Windows approval shortcut target", () => {
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

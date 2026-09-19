import { describe, expect, it } from "vitest";
import { getToolPresentation, parseChecklistResult } from "./toolPresentation";

describe("tool card presentations", () => {
  it("presents discovery with the requested tool names", () => {
    const card = getToolPresentation("ToolSearch", { names: ["Browser", "WebSearch"] });
    expect(card).toMatchObject({ known: true, kind: "search", label: "加载工具", summary: "Browser · WebSearch" });
    expect(card.fields[0].label).toBe("工具名称");
  });
  it("builds a readable file edit summary and structured fields", () => {
    const card = getToolPresentation("Edit", {
      file_path: "src/App.tsx",
      old_string: "before",
      new_string: "after",
      replace_all: false,
    });
    expect(card).toMatchObject({ kind: "file", label: "编辑文件", summary: "src/App.tsx" });
    expect(card.fields.map((field) => field.label)).toEqual(["文件", "全部替换", "替换前", "替换后"]);
  });

  it("keeps unknown plugin tools on the generic fallback", () => {
    const card = getToolPresentation("acme.custom_tool", { target: "demo" });
    expect(card).toMatchObject({ known: false, kind: "generic", label: "acme.custom_tool" });
    expect(card.fields[0]).toMatchObject({ label: "target", value: "demo" });
  });

  it("presents ApplyPatch as a file edit with affected paths", () => {
    const card = getToolPresentation("apply_patch", {
      affected_paths: ["src/a.ts", "src/b.ts"],
      patch: "*** Begin Patch\n*** End Patch",
    });
    expect(card).toMatchObject({ kind: "file", label: "应用补丁", summary: "src/a.ts · src/b.ts" });
    expect(card.fields[0]).toMatchObject({ key: "affected_paths", label: "影响文件" });
  });

  it("parses checklist output into progress cards", () => {
    expect(parseChecklistResult("Checklist created:\n  📋 Release\n  ✅ 1. Build\n  ◻ 2. Ship\n  (1/2 completed)"))
      .toEqual([{ title: "Release", items: [{ text: "Build", checked: true }, { text: "Ship", checked: false }], done: 1, total: 2 }]);
  });
});

import { describe, expect, it } from "vitest";
import { presentUserMessage } from "./userPromptDisplay";

describe("presentUserMessage", () => {
  it("lifts file, folder, and document references out of the prompt", () => {
    const presented = presentUserMessage([
      '<file path="/work/notes.md"></file>',
      '<file name="draft.txt">\nhello &amp; more\n</file>',
      "<folder>\n/work/src\n</folder>",
      "<document-reference>\n文档：说明书\n位置：第 2 页 [1-4]\n\n引用正文\n</document-reference>",
      "看看这张图",
    ].join("\n\n"));

    expect(presented.text).toBe("看看这张图");
    expect(presented.attachments).toEqual([
      { kind: "file", label: "notes.md", detail: "仅路径", title: "/work/notes.md" },
      { kind: "file", label: "draft.txt", title: "draft.txt" },
      { kind: "folder", label: "src", title: "/work/src" },
      { kind: "document", label: "说明书", detail: "第 2 页 [1-4]" },
    ]);
    expect(presented.text).not.toContain("引用正文");
  });

  it("lifts IDE context the same way and leaves plain text untouched", () => {
    const presented = presentUserMessage([
      "<crabcode-ide-context>",
      JSON.stringify({
        current_file: { path: "/workspace/src/app.ts" },
        references: [{ kind: "folder", path: "/workspace/src/components" }],
      }),
      "</crabcode-ide-context>",
      "",
      "检查引用",
    ].join("\n"));

    expect(presented.text).toBe("检查引用");
    expect(presented.attachments).toEqual([
      { kind: "ide", label: "app.ts", detail: "当前文件", title: "/workspace/src/app.ts" },
      { kind: "folder", label: "components", title: "/workspace/src/components" },
    ]);
    expect(presentUserMessage("hello")).toEqual({ text: "hello", attachments: [] });
  });
});

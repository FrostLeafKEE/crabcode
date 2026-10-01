import { describe, expect, it } from "vitest";
import {
  parsePromptTemplateFile,
  promptTemplateFilename,
  serializePromptTemplate,
  serializePromptTemplates,
} from "./promptTemplateFile";

describe("prompt template JSON", () => {
  it("round-trips one template and drops blank sections", () => {
    const text = serializePromptTemplate({
      id: "care",
      name: " 客服 ",
      sections: { intro: " 先给结论 ", extra: "  ", system: "" },
    });
    expect(parsePromptTemplateFile(text)).toEqual([{
      id: "care",
      name: "客服",
      sections: { intro: "先给结论" },
    }]);
  });

  it("accepts a list, a templates object, and a settings fragment", () => {
    const template = { name: "值班", sections: { system: "用中文" } };
    expect(parsePromptTemplateFile(JSON.stringify([template]))).toEqual([template]);
    expect(parsePromptTemplateFile(serializePromptTemplates([template]))).toEqual([template]);
    expect(parsePromptTemplateFile(JSON.stringify({
      prompt_templates: [{ id: "night", name: "夜班", sections: {} }],
    }))).toEqual([{ id: "night", name: "夜班", sections: {} }]);
  });

  it("rejects invalid files", () => {
    expect(() => parsePromptTemplateFile("{")).toThrow("不是有效的 JSON");
    expect(() => parsePromptTemplateFile("{}")).toThrow("JSON 需要是模版对象");
    expect(() => parsePromptTemplateFile("[]")).toThrow("JSON 里没有提示词模版");
    expect(() => parsePromptTemplateFile(JSON.stringify({ name: "默认", sections: {} }))).toThrow("不能是「默认」");
    expect(() => parsePromptTemplateFile(JSON.stringify([
      { name: "客服", sections: {} },
      { name: "客服", sections: {} },
    ]))).toThrow("在文件里重复");
    expect(() => parsePromptTemplateFile(JSON.stringify({
      name: "客服",
      sections: { intro: "x".repeat(20_001) },
    }))).toThrow("过长");
  });

  it("builds a safe json filename", () => {
    expect(promptTemplateFilename("客服/夜班")).toBe("客服 夜班.json");
    expect(promptTemplateFilename("CON")).toBe("prompt-template.json");
    expect(promptTemplateFilename("prompt-templates")).toBe("prompt-templates.json");
  });
});

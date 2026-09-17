import * as path from "path";

export interface IdeContextSnapshot {
  active_file: string;
  selected_text?: string | null;
  cursor_line?: number | null;
  cursor_column?: number | null;
  open_files?: string[];
  language_id?: string | null;
}

export interface IdePathReference {
  kind: "file" | "folder";
  path: string;
  name?: string;
}

const START = "<crabcode-ide-context>";
const END = "</crabcode-ide-context>";

function safeJson(value: unknown): string {
  return JSON.stringify(value, null, 2)
    .replace(/</g, "\\u003c")
    .replace(/>/g, "\\u003e")
    .replace(/&/g, "\\u0026");
}

function referencePayload(
  context: IdeContextSnapshot | null,
  references: IdePathReference[],
): Record<string, unknown> {
  const payload: Record<string, unknown> = {};
  if (context?.active_file) {
    payload.current_file = {
      path: context.active_file,
      language_id: context.language_id ?? null,
      cursor: context.cursor_line == null
        ? null
        : {
            line: context.cursor_line + 1,
            column: (context.cursor_column ?? 0) + 1,
          },
      selected_text: context.selected_text || null,
      visible_files: Array.isArray(context.open_files) ? context.open_files : [],
    };
  }
  if (references.length > 0) {
    payload.references = references.map((reference) => ({
      kind: reference.kind,
      path: reference.path,
    }));
  }
  return payload;
}

export function buildIdeContextPrompt(
  userText: string,
  context: IdeContextSnapshot | null,
  references: IdePathReference[],
): string {
  const payload = referencePayload(context, references);
  if (Object.keys(payload).length === 0) return userText;
  const envelope = `${START}\n${safeJson(payload)}\n${END}`;
  return userText.trim() ? `${envelope}\n\n${userText}` : envelope;
}

export function displayIdeContextPrompt(prompt: string): string {
  const start = prompt.indexOf(START);
  if (start < 0) return prompt;
  const end = prompt.indexOf(END, start + START.length);
  if (end < 0) return prompt;
  const json = prompt.slice(start + START.length, end).trim();
  const body = `${prompt.slice(0, start)}${prompt.slice(end + END.length)}`.trim();
  const labels: string[] = [];
  try {
    const payload = JSON.parse(json) as {
      current_file?: { path?: unknown };
      references?: Array<{ kind?: unknown; path?: unknown }>;
    };
    if (typeof payload.current_file?.path === "string") {
      labels.push(`[IDE：${path.basename(payload.current_file.path)}]`);
    }
    for (const reference of payload.references ?? []) {
      if (typeof reference.path !== "string") continue;
      labels.push(`[${reference.kind === "folder" ? "文件夹" : "文件"}：${path.basename(reference.path)}]`);
    }
  } catch {
    return body || "[IDE 上下文]";
  }
  const summary = labels.join(" ") || "[IDE 上下文]";
  return body ? `${summary}\n\n${body}` : summary;
}

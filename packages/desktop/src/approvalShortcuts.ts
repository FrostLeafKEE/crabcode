import type { ChatItem, SessionViewState } from "./types";

export const APPROVE_SHORTCUT = "Ctrl+Alt+Shift+F9";
export const DENY_SHORTCUT = "Ctrl+Alt+Shift+F10";

export interface PendingApproval {
  sessionKey: string;
  item: ChatItem;
}

export function uniquePendingApproval(sessions: Record<string, SessionViewState>): PendingApproval | null {
  const pending = Object.entries(sessions).flatMap(([sessionKey, session]) =>
    session.connected
      ? session.items.filter((item) => item.kind === "permission" && item.status === "pending")
        .map((item) => ({ sessionKey, item }))
      : [],
  );
  return pending.length === 1 && pending[0].item.tool_use_id ? pending[0] : null;
}

export function supportsWindowsApprovalShortcuts(): boolean {
  return typeof navigator !== "undefined" && /Windows/i.test(navigator.userAgent);
}

/* @vitest-environment jsdom */
import { act, useState } from "react";
import { createRoot } from "react-dom/client";
import { describe, expect, it } from "vitest";
import { ConversationTurns, type ProcessPresentation } from "./ConversationTurns";
import { applyGatewayEvent } from "./events";
import { normalizeSettings } from "./native";
import type { ChatItem, DesktopSettings, SessionViewState } from "./types";

(globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
function fixture(): ChatItem[] {
  return [
    { id: 'u', kind: 'user', text: 'task' },
    { id: 'pre', kind: 'assistant', text: 'checking', status: 'complete' },
    { id: 'tool', kind: 'tool', status: 'complete', result: 'ok' },
    { id: 'f', kind: 'assistant', text: 'finished', status: 'complete', messageUuid: 'final' },
    { id: 'edits', kind: 'file_edit_summary' },
    { id: 'end', kind: 'turn_duration', durationMs: 83000, turnEndReason: 'end_turn', finalMessageId: 'final' },
  ];
}
function StatefulItem({ item, presentation }: { item: ChatItem; presentation: ProcessPresentation }) {
  const [open, setOpen] = useState(false);
  return <button {...presentation} data-item={item.id} onClick={() => setOpen(!open)}>{item.text || item.kind}{open ? ':open' : ''}</button>;
}
function harness(sessionKey: string) {
  const node = document.createElement('div');
  node.className = 'messages';
  document.body.append(node);
  const root = createRoot(node);
  const render = (items: ChatItem[], enabled = true, busy = false, key = sessionKey, showToolCallCount = false) => act(() => root.render(
    <ConversationTurns items={items} busy={busy} sessionKey={key} enabled={enabled} showDuration showToolCallCount={showToolCallCount} durationFormat="hms"
      renderItem={(item, presentation) => <StatefulItem item={item} presentation={presentation} />} />));
  const hidden = (id: string) => node.querySelector<HTMLElement>(`[data-process-entry="${id}"]`)?.hidden;
  const close = () => { act(() => root.unmount()); node.remove(); };
  return { node, render, hidden, close };
}

describe('automatic process disclosure', () => {
  it('defaults on for existing settings and respects an explicit false', () => {
    expect(normalizeSettings({ connections: [] } as unknown as DesktopSettings).auto_collapse_process).toBe(true);
    expect(normalizeSettings({ connections: [], auto_collapse_process: false } as unknown as DesktopSettings).auto_collapse_process).toBe(false);
    expect(normalizeSettings({ connections: [] } as unknown as DesktopSettings).show_tool_call_count).toBe(false);
    expect(normalizeSettings({ connections: [], show_tool_call_count: true } as unknown as DesktopSettings).show_tool_call_count).toBe(true);
  });

  it('folds only the completed process and preserves card state across setting changes', () => {
    const h = harness('completed');
    try {
      h.render(fixture());
      expect(h.hidden('pre')).toBe(true);
      expect(h.hidden('tool')).toBe(true);
      for (const id of ['u', 'f', 'edits']) expect(h.hidden(id)).toBe(false);
      expect(h.node.querySelector('.process-summary')?.textContent).toContain('1分23秒');
      expect(h.node.querySelector('.process-summary')?.textContent).toContain('已完成');
      expect(h.node.querySelector('.process-summary')?.textContent).not.toContain('次工具调用');
      h.render(fixture(), true, false, 'completed', true);
      expect(h.node.querySelector('.process-summary')?.textContent).toContain('1次工具调用');
      expect(h.hidden('tool')).toBe(true);
      h.render(fixture());
      expect(h.node.querySelector('.process-summary')?.textContent).not.toContain('次工具调用');
      act(() => h.node.querySelector<HTMLButtonElement>('.process-summary')!.click());
      expect(h.hidden('tool')).toBe(false);
      act(() => h.node.querySelector<HTMLButtonElement>('[data-item="tool"]')!.click());
      h.render(fixture(), false);
      expect(h.node.querySelector('.process-summary')).toBeNull();
      expect(h.hidden('pre')).toBe(false);
      h.render(fixture());
      expect(h.hidden('tool')).toBe(false);
      expect(h.node.querySelector('[data-item="tool"]')?.textContent).toBe('tool:open');
      h.render([], true, false, 'other');
      h.render(fixture().map(item => item.id === 'u' ? { ...item, id: 'durable-u' } : item));
      expect(h.hidden('tool')).toBe(false);
    } finally { h.close(); }
  });

  it('shows running and interrupted turns and keeps approvals and pictures available when manually folded', () => {
    const h = harness('interruptions');
    try {
      h.render(fixture().slice(0, -2), true, true);
      expect(h.hidden('pre')).toBe(false);
      expect(h.node.querySelector('.process-summary')?.textContent).toContain('处理中');
      h.render(fixture().map(item => item.kind === 'turn_duration' ? { ...item, turnEndReason: 'interrupted' } : item));
      expect(h.hidden('tool')).toBe(false);
      const items = fixture();
      items.splice(3, 0, { id: 'approval', kind: 'permission', status: 'pending' },
        { id: 'picture', kind: 'tool', images: [{ data: 'YQ==', media_type: 'image/png' }], status: 'complete' });
      h.render(items);
      act(() => h.node.querySelector<HTMLButtonElement>('.process-summary')!.click());
      expect(h.hidden('tool')).toBe(true);
      expect(h.hidden('approval')).toBe(false);
      expect(h.hidden('picture')).toBe(false);
    } finally { h.close(); }
  });

  it('defers automatic collapse while process text is selected', () => {
    const h = harness('selection');
    try {
      h.render(fixture().slice(0, -1), true, true);
      const range = document.createRange();
      range.selectNodeContents(h.node.querySelector('[data-item="pre"]')!);
      act(() => { window.getSelection()!.addRange(range); document.dispatchEvent(new Event('selectionchange')); });
      h.render(fixture());
      expect(h.hidden('pre')).toBe(false);
      act(() => { window.getSelection()!.removeAllRanges(); document.dispatchEvent(new Event('selectionchange')); });
      expect(h.hidden('pre')).toBe(true);
    } finally { h.close(); }
  });

  it('retains terminal reasons and steering origins in the actual event projection', () => {
    const state: SessionViewState = { id: 's', cwd: '/work', title: 'test', items: fixture().slice(0, -1),
      busy: true, connected: true, loading: false, error: null, operationId: 'op', status: null, runStartedAt: 1 };
    const stopped = applyGatewayEvent(state, { type: 'turn_complete', reason: 'interrupted', assistant_message_uuid: 'final' });
    expect(stopped.items.at(-1)).toMatchObject({ kind: 'turn_duration', turnEndReason: 'interrupted', finalMessageId: 'final' });
    const history = applyGatewayEvent(state, { type: 'session_history', messages: [
      { uuid: 'u', role: 'user', content: 'task' },
      { uuid: 'steer', role: 'user', origin: 'user-steering', content: 'correction' },
      { uuid: 'f', role: 'assistant', content: 'answer', turn_details: { session_id: 's', reason: 'interrupted' } },
    ] });
    expect(history.items.find(item => item.id === 'steer')?.origin).toBe('user-steering');
    const h = harness('projection');
    try { h.render(history.items); expect(h.node.querySelectorAll('.conversation-turn')).toHaveLength(1); }
    finally { h.close(); }
  });
});

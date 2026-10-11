export interface ProcessItem {
  id: string;
  kind: string;
  text?: string;
  origin?: string | null;
  followUpMode?: string;
  messageUuid?: string | null;
  images?: readonly unknown[];
  status?: string;
  agent_id?: string | null;
  tool_use_id?: string;
  isError?: boolean;
  turnEndReason?: string;
  finalMessageId?: string;
  durationMs?: number;
  turnDetails?: { reason?: string | null; duration_ms?: number | null } | null;
}
export interface TurnProcess {
  processIds: string[];
  finalMessageId: string | null;
  canAutoCollapse: boolean;
  toolCount: number;
  errorCount: number;
  durationMs: number | null;
  completed: boolean;
  reason?: string | null;
}
export interface ProcessTurn<T extends ProcessItem = ProcessItem> extends TurnProcess {
  id: string;
  items: T[];
}
export function createTurnProcessPolicy(): {
  startsTurn(item: ProcessItem): boolean;
  describeTurn(items: ProcessItem[], options?: { completed?: boolean; reason?: string | null; finalMessageId?: string | null }): TurnProcess;
  groupTurns<T extends ProcessItem>(items: T[], busy?: boolean): ProcessTurn<T>[];
  isCollapsed(turn: TurnProcess, enabled: boolean, manual?: boolean, interacting?: boolean): boolean;
  viewKeys(sessionKey: string, turn: { id: string; finalMessageId?: string | null }): string[];
};

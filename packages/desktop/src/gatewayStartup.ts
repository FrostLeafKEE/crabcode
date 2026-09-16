import type { WorkspaceInfo } from "./types";

export function gatewayEnvironmentLog(workspace: WorkspaceInfo): string[] {
  const runtime = workspace.runtime;
  return [
    ...(runtime ? [
      `CrabCode Gateway 版本：${runtime.gateway_version}`,
      `Gateway 包路径：${runtime.gateway_path}`,
      `Gateway 使用的 Python：${runtime.python_version}`,
      `Python 解释器路径：${runtime.python_executable}`,
      `Python 环境：${({ venv: "虚拟环境", conda: "Conda 环境", system: "基础环境" })[runtime.environment_kind]} · ${runtime.python_prefix}`,
      `Gateway 运行平台：${runtime.platform}`,
    ] : ["此 Gateway 未提供详细运行环境信息（可能为旧版本服务）"]),
    `Gateway 启动目录：${workspace.startup_cwd}`,
  ];
}

export function gatewayLogAddress(baseUrl: string): string {
  const url = new URL(baseUrl);
  url.username = "";
  url.password = "";
  url.search = "";
  url.hash = "";
  return url.toString();
}

export interface GatewayStartupProgress {
  connectionId: string;
  operationId: string;
  stage: string;
  detail: string;
}

export interface GatewayStartupState {
  stage: string;
  detail: string;
  startedAt: number;
  finishedAt: number | null;
  history: Array<{ time: number; detail: string }>;
}

export function updateGatewayStartup(
  current: GatewayStartupState | undefined,
  stage: string,
  detail: string,
  now = Date.now(),
): GatewayStartupState {
  const history = current?.history ?? [];
  return {
    ...current,
    stage,
    detail,
    startedAt: current?.startedAt ?? now,
    finishedAt: stage === "online" || stage === "error" ? now : null,
    history: history.at(-1)?.detail === detail
      ? history
      : [...history, { time: now, detail }].slice(-100),
  };
}

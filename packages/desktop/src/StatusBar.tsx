import { AlertTriangle, Check, ChevronUp, Clock, Folder, LoaderCircle, MonitorUp, MousePointer2, Power, RefreshCw, Server, Terminal, WifiOff, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { ComputerUseState } from "./computerUse";
import type { GatewayStartupState } from "./gatewayStartup";
import type { ConnectionPreset, GatewayViewState, ProjectPreset } from "./types";

interface StatusBarProps {
  connection?: ConnectionPreset | null;
  gateway?: GatewayViewState | null;
  startup?: GatewayStartupState;
  project?: ProjectPreset | null;
  loading?: boolean;
  error?: string | null;
  activity?: string | null;
  onRetry?: () => void;
  onConnections?: () => void;
  computerUse?: ComputerUseState;
  onComputerUseEnabledChange?: (enabled: boolean) => void;
  onComputerUseOpenInputSettings?: () => void;
  onComputerUseRefresh?: () => void;
}

export function StatusBar({ connection, gateway, startup, project, loading, error, activity, onRetry, onConnections, computerUse, onComputerUseEnabledChange, onComputerUseOpenInputSettings, onComputerUseRefresh }: StatusBarProps) {
  const [expanded, setExpanded] = useState(false);
  const [computerExpanded, setComputerExpanded] = useState(false);
  const [now, setNow] = useState(Date.now);
  const [mountedAt] = useState(Date.now);
  const logRef = useRef<HTMLDivElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
  const computerToggleRef = useRef<HTMLButtonElement>(null);
  const failed = Boolean(error || gateway?.status === "error");
  const busy = !failed && Boolean(loading || activity || (connection && (!gateway || gateway.status === "connecting")));
  const status = failed ? "error" : busy ? "busy" : gateway?.status === "online" ? "online" : "offline";
  const detail = error || gateway?.error || activity || (loading ? "正在读取桌面配置…"
    : busy ? startup?.detail || "正在连接 Gateway…"
    : gateway?.status === "online" ? "就绪" : "尚未连接 Gateway");
  const elapsed = Math.max(0, Math.floor(((startup?.finishedAt ?? now) - (startup?.startedAt ?? mountedAt)) / 1000));
  const duration = elapsed < 60 ? `${elapsed} 秒` : `${Math.floor(elapsed / 60)} 分 ${elapsed % 60} 秒`;

  useEffect(() => {
    if (!busy) return;
    setNow(Date.now());
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [busy]);

  useEffect(() => {
    if (expanded && logRef.current) logRef.current.scrollTop = logRef.current.scrollHeight;
  }, [expanded, startup?.history, activity]);

  const close = () => {
    setExpanded(false);
    toggleRef.current?.focus();
  };
  const computerStatusLabel = computerUse?.status === "ready"
    ? computerUse.capabilities?.input_available === false ? "有限可用" : "可用"
    : computerUse?.status === "busy" ? "Agent 正在操作"
      : computerUse?.status === "connecting" ? "正在连接"
        : computerUse?.status === "unavailable" ? "不可用"
          : computerUse?.status === "error" ? "连接错误" : "已关闭";
  const frame = computerUse?.latestFrame;
  const cursor = computerUse?.cursor;
  const needsMacInputPermission = computerUse?.enabled
    && computerUse.capabilities?.platform === "macos"
    && computerUse.capabilities.input_available === false;
  const cursorLeft = frame && cursor ? (cursor.x - frame.origin_x) / frame.width * 100 : -1;
  const cursorTop = frame && cursor ? (cursor.y - frame.origin_y) / frame.height * 100 : -1;

  return (
    <footer className={`desktop-status-bar ${status}`} aria-label="应用状态栏" onKeyDown={(event) => {
      if (event.key === "Escape" && (expanded || computerExpanded)) {
        event.stopPropagation();
        if (computerExpanded) {
          setComputerExpanded(false);
          computerToggleRef.current?.focus();
        } else close();
      }
    }}>
      {expanded && (
        <section className="startup-details" id="startup-details" aria-label="启动详情">
          <header>
            <strong><Terminal />{connection?.name ?? "Crab Desktop"} · 启动详情</strong>
            <span>{startup ? `耗时 ${duration}` : ""}</span>
            {failed && onRetry && <button className="status-retry" onClick={onRetry}><RefreshCw />{connection ? "重试连接" : "重新加载"}</button>}
            <button className="icon-button tiny" aria-label="关闭启动详情" onClick={close}><X /></button>
          </header>
          <div className="startup-log" ref={logRef}>
            {startup?.history.length ? startup.history.map((entry, index) => (
              <div className="startup-log-line" key={`${entry.time}-${index}`}>
                <time>{new Date(entry.time).toLocaleTimeString("zh-CN", { hour12: false })}</time>
                <span>{entry.detail}</span>
              </div>
            )) : <p>{detail}</p>}
            {activity && startup?.history.length ? <p role="status">{activity}</p> : null}
          </div>
        </section>
      )}
      {computerExpanded && computerUse && (
        <section className="computer-use-console" id="computer-use-console" aria-label="Computer Use 控制台">
          <header>
            <strong><MonitorUp />Computer Use</strong>
            <span className={`computer-use-state ${computerUse.status}`}>{computerStatusLabel}</span>
            <button
              className={`computer-use-power ${computerUse.enabled ? "enabled" : ""}`}
              onClick={() => onComputerUseEnabledChange?.(!computerUse.enabled)}
              title={computerUse.enabled ? "关闭 Computer Use" : "开启 Computer Use"}
            >
              <Power />{computerUse.enabled ? "关闭" : "开启"}
            </button>
            <button className="icon-button tiny" aria-label="关闭 Computer Use 控制台" onClick={() => setComputerExpanded(false)}><X /></button>
          </header>
          <div className="computer-use-preview">
            {frame ? (
              <div className="computer-use-frame">
                <img src={`data:${frame.media_type};base64,${frame.data}`} alt="Agent 当前看到的桌面" />
                {cursor && cursorLeft >= 0 && cursorLeft <= 100 && cursorTop >= 0 && cursorTop <= 100 && (
                  <MousePointer2
                    className="computer-use-cursor"
                    style={{ left: `${cursorLeft}%`, top: `${cursorTop}%` }}
                    aria-label={`光标 ${cursor.x}, ${cursor.y}`}
                  />
                )}
              </div>
            ) : (
              <div className="computer-use-empty">
                <MonitorUp />
                <span>{computerUse.enabled
                  ? needsMacInputPermission ? "等待 Agent 开始操作电脑" : computerUse.error || "等待 Agent 开始操作电脑"
                  : "Computer Use 已关闭，不会向 Agent 暴露相关工具"}</span>
              </div>
            )}
          </div>
          {needsMacInputPermission && (
            <section className="computer-use-permission" aria-label="需要辅助功能权限">
              <AlertTriangle />
              <div>
                <strong>需要开启辅助功能权限</strong>
                <span>请在 macOS“系统设置 → 隐私与安全性 → 辅助功能”中允许 Crab Desktop，Agent 才能点击、输入和滚动。当前仍可查看屏幕和启动应用。</span>
              </div>
              <div className="computer-use-permission-actions">
                <button type="button" onClick={onComputerUseOpenInputSettings}>打开系统设置</button>
                <button type="button" onClick={onComputerUseRefresh}>重新检测</button>
              </div>
            </section>
          )}
          {computerUse.enabled && computerUse.capabilities?.input_available === false && !needsMacInputPermission && (
            <p className="computer-use-warning">{computerUse.capabilities.reason || "桌面输入权限不可用；Agent 仍可查看屏幕和启动应用。"}</p>
          )}
          <div className="computer-use-log" aria-label="Agent 操作记录">
            {computerUse.logs.length ? [...computerUse.logs].reverse().map((entry) => (
              <div className={`computer-use-log-line ${entry.ok ? "ok" : "failed"}`} key={entry.id}>
                <time>{new Date(entry.time).toLocaleTimeString("zh-CN", { hour12: false })}</time>
                <code>{entry.action}</code>
                <span>{entry.summary}</span>
              </div>
            )) : <p>还没有 Computer Use 操作。</p>}
          </div>
        </section>
      )}
      {connection && (
        <button className="status-connection" onClick={onConnections} title={`管理连接 · ${connection.name}`}>
          <Server /><span>{connection.name}</span>
        </button>
      )}
      <button
        className="status-current"
        ref={toggleRef}
        onClick={() => {
          setComputerExpanded(false);
          setExpanded((value) => !value);
        }}
        aria-expanded={expanded}
        aria-controls="startup-details"
        title={`${detail}\n点击查看启动详情`}
      >
        {busy ? <LoaderCircle className="spin" /> : failed ? <AlertTriangle /> : status === "online" ? <Check /> : <WifiOff />}
        <span className="status-message" role="status" aria-live="polite">{detail}</span>
        {busy && !activity && <span className="status-elapsed"><Clock />{duration}</span>}
        <ChevronUp className={`status-expand ${expanded ? "expanded" : ""}`} />
      </button>
      {failed && onRetry && <button className="status-retry" onClick={onRetry} title={connection ? "重新连接 Gateway" : "重新加载桌面配置"}><RefreshCw />重试</button>}
      <div className="status-spacer" />
      {project && <span className="status-project" title={project.path}><Folder />{project.name}</span>}
      {computerUse && (
        <button
          className={`status-computer-use ${computerUse.status}`}
          ref={computerToggleRef}
          onClick={() => {
            setExpanded(false);
            setComputerExpanded((value) => !value);
          }}
          aria-expanded={computerExpanded}
          aria-controls="computer-use-console"
          title={`Computer Use · ${computerStatusLabel}\n点击查看 Agent 对电脑的操作`}
        >
          <span>Computer Use</span>
          <span className="computer-use-dot" />
        </button>
      )}
    </footer>
  );
}

import { AlertTriangle, Check, ChevronUp, Clock, Folder, LoaderCircle, RefreshCw, Server, Terminal, WifiOff, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
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
}

export function StatusBar({ connection, gateway, startup, project, loading, error, activity, onRetry, onConnections }: StatusBarProps) {
  const [expanded, setExpanded] = useState(false);
  const [now, setNow] = useState(Date.now);
  const [mountedAt] = useState(Date.now);
  const logRef = useRef<HTMLDivElement>(null);
  const toggleRef = useRef<HTMLButtonElement>(null);
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

  return (
    <footer className={`desktop-status-bar ${status}`} aria-label="应用状态栏" onKeyDown={(event) => {
      if (event.key === "Escape" && expanded) {
        event.stopPropagation();
        close();
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
      {connection && (
        <button className="status-connection" onClick={onConnections} title={`管理连接 · ${connection.name}`}>
          <Server /><span>{connection.name}</span>
        </button>
      )}
      <button
        className="status-current"
        ref={toggleRef}
        onClick={() => setExpanded((value) => !value)}
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
    </footer>
  );
}

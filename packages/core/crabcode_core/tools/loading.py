"""Provider-independent tool discovery. State belongs to a conversation, not a Tool."""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from typing import Callable

from crabcode_core.types.config import ToolLoadingSettings
from crabcode_core.types.tool import Tool, ToolContext

CORE_TOOLS = (
    "Bash", "Read", "apply_patch", "Edit", "Write", "Grep", "Glob", "Lint",
    "AskUser", "Checklist", "SwitchMode", "Skill", "ToolSearch",
)
GROUPS = {
    "web": ("WebSearch", "Browser"),
    "images": ("Image", "ImageGenerate"),
    "computer": ("ComputerUse",),
    "agents": ("Agent", "AgentStatus", "AgentWait", "AgentCancel", "AgentSendInput"),
    "sessions": ("ListAgents", "SendMessage"),
    "teams": ("TeamCreate", "TeamSpawn", "TeamMessage", "TeamBroadcast", "TeamStatus",
              "TeamTaskAdd", "TeamTaskClaim", "TeamTaskComplete", "TeamShutdown"),
    "background": ("Monitor", "TaskList", "TaskStop"),
    "schedule": ("ScheduleCreate", "ScheduleList", "ScheduleCancel", "ScheduleStatus",
                 "SchedulePause", "ScheduleResume", "ScheduleRun"),
    "memory": ("Memory",),
    "checkpoints": ("Checkpoint", "Revert"),
    "goals": ("create_goal", "get_goal", "update_goal"),
}
ALIASES = {
    "web": "search browse internet 网页 浏览 搜索 联网",
    "images": "image picture generate 图片 生成 看图",
    "computer": "computer desktop gui screen mouse keyboard 电脑 桌面 图形界面 鼠标 键盘 操作",
    "agents": "delegate subagent 子代理 委派",
    "sessions": "peer message 会话 消息",
    "teams": "team coordinate 团队 协作",
    "background": "monitor task stop 监控 后台 停止",
    "schedule": "cron scheduled 定时 调度",
    "memory": "remember memories 记忆 记住",
    "checkpoints": "checkpoint revert undo 快照 回滚 撤销",
    "goals": "goal objective 目标",
}


@dataclass
class ToolLoadingState:
    # Append-only order preserves existing schema ordering as tools are discovered.
    names: list[str] = field(default_factory=list)

    @classmethod
    def restore(cls, value: object) -> ToolLoadingState:
        if not isinstance(value, list):
            return cls()
        return cls(list(dict.fromkeys(n for n in value if isinstance(n, str))))


class ToolCatalog:
    def __init__(
        self, tools: list[Tool], context: ToolContext, settings: ToolLoadingSettings,
        state: ToolLoadingState, *, plan_mode: bool = False,
        on_change: Callable[[list[str]], None] | None = None,
        pinned: tuple[str, ...] = (),
    ) -> None:
        # Restrict to this agent's registry and the current model/mode. Discovery
        # never grants permissions or reaches into the parent agent's registry.
        self.tools: dict[str, Tool] = {}
        for tool in tools:
            if tool.is_available(context) and (not plan_mode or tool.is_read_only):
                self.tools.setdefault(tool.name, tool)
        self.settings = settings
        self.mode = settings.mode if "ToolSearch" in self.tools else "eager"
        self.state = state
        self.on_change = on_change
        initial = tuple(self.tools) if self.mode == "eager" else (
            *CORE_TOOLS, *settings.pinned_tools, *pinned,
        )
        self.load(initial)
        self.exposed_names = frozenset(tool.name for tool in self.loaded)

    def load(self, names) -> list[str]:
        selected = [n for n in dict.fromkeys(names) if n in self.tools]
        added = [n for n in selected if n not in self.state.names]
        if added:
            self.state.names.extend(added)
            if self.on_change:
                self.on_change(list(self.state.names))
        return selected

    @property
    def loaded(self) -> list[Tool]:
        return [self.tools[n] for n in self.state.names if n in self.tools]

    def group(self, name: str) -> str:
        if name in CORE_TOOLS:
            return "core"
        for group, names in GROUPS.items():
            if name in names:
                return group
        server = getattr(self.tools[name], "_server_name", None)
        return f"mcp:{server}" if server else "extensions"

    def directory(self) -> str:
        if self.mode == "eager":
            return ""
        groups: dict[str, list[str]] = {}
        for name in self.tools:
            if name not in CORE_TOOLS:
                groups.setdefault(self.group(name), []).append(name)
        # Large MCP registries stay compact; ToolSearch list pagination is the
        # lossless fallback, so truncation here cannot hide a tool permanently.
        lines = [
            "# Tool discovery",
            "The names below are NOT callable yet and do not include their schemas. When a task needs one, "
            "first make a real ToolSearch call using its exact name, group or a query. Never guess its "
            "arguments or print a textual/pseudo tool call. A loaded tool becomes callable in the NEXT "
            "response only. Use list=true to browse all tools.",
        ]
        for group, names in sorted(groups.items())[:20]:
            summary = ", ".join(names[:12])
            if len(names) > 12:
                summary += f" (+{len(names) - 12} more)"
            line = f"{group}: {summary}"
            if sum(map(len, lines)) + len(line) > 1800:
                lines.append("More tools available through ToolSearch list=true.")
                break
            lines.append(line)
        if len(groups) > 20:
            lines.append("More groups available through ToolSearch list=true.")
        return "\n".join(lines)

    def instructions(self) -> str:
        servers = {}
        for tool in self.loaded:
            instructions = getattr(tool, "server_instructions", "")
            if instructions:
                servers[getattr(tool, "_server_name", tool.name)] = instructions
        return "\n\n".join(f"MCP {name}:\n{value}" for name, value in sorted(servers.items()))

    def search(self, *, names: list[str], group: str, query: str,
               list_only: bool, offset: int, limit: int) -> str:
        candidates = list(self.tools)
        if names:
            missing = [n for n in names if n not in self.tools]
            if missing:
                return json.dumps({"error": "Unknown or unavailable tools", "names": missing,
                                   "hint": "Use list=true to browse available tools."})
            candidates = list(dict.fromkeys(names))
        if group:
            candidates = [n for n in candidates if self.group(n) == group]
        if query:
            terms = re.findall(r"[\w]+", query.casefold())
            def score(name: str) -> int:
                tool = self.tools[name]
                text = f"{name} {tool.description} {getattr(tool, '_cached_prompt', '')} "
                text += f"{self.group(name)} {ALIASES.get(self.group(name), '')}"
                return sum(term in text.casefold() for term in terms)
            candidates = sorted((n for n in candidates if score(n)), key=lambda n: -score(n))
        page = candidates[offset:offset + limit]
        loaded = [] if list_only else self.load(page)
        return json.dumps({
            "tools": [{"name": n, "group": self.group(n),
                       "summary": (self.tools[n].description or getattr(self.tools[n], '_cached_prompt', ''))[:160],
                       "loaded": n in self.state.names} for n in page],
            "loaded": loaded, "total": len(candidates),
            "next_offset": offset + limit if offset + limit < len(candidates) else None,
            "hint": "Loaded schemas are available in the next response. Permissions still apply."
                    if loaded else "Use names or group to load; list=true with offset to browse.",
        }, ensure_ascii=False)

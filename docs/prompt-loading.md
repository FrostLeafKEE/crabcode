# Prompt 与工具按需加载

默认启用 `discovery`：常用编码工具、AskUser、Checklist、SwitchMode、ToolSearch 常驻；安装了 Skill 时，Skill 的简要目录也常驻。Web、图片、Agent、Team、后台任务、调度、记忆、快照、Goal、MCP 和额外工具通过 ToolSearch 加载。

```json
{
  "tool_loading": {
    "mode": "discovery",
    "pinned_tools": ["WebSearch"]
  }
}
```

兼容或排障时，将 `mode` 改为 `eager` 即恢复全量工具说明。配置遵循现有全局/项目 settings 合并规则；已初始化的进程需要重新加载配置或重启。

同一会话的加载集合只增不减。从 eager 切回 discovery 后，如果希望重新获得最小首轮集合，请新建会话。

## 加载与安全边界

- ToolSearch 接受 `names`、`group` 或 `query`；`list: true` 只列目录，`offset`/`limit` 分页。未指定搜索条件时也只列目录。
- 工具完整 schema 从**下一次模型请求**起发送。搜索结果只返回名称、摘要和加载状态，不再重复整个 schema。
- 加载不执行工具、不授予权限。实际调用仍检查本轮暴露集合、当前可用性、Plan 模式、输入、权限和 hooks。
- 已加载名称以稳定追加顺序保存到会话元数据，并随 resume/fork 恢复；新会话重置。压缩不清空它。工具删除、禁用、模型能力变化、子代理 allowlist 会重新过滤可用集合。
- 子代理独立保存自己的加载集合。自定义受限工具集未包含 ToolSearch 时使用全量可用集合，避免失去唯一的发现入口。
- 活动 Goal、已有 Agent/Monitor 的管理工具，以及 Ultra 模式的 Agent 工具自动常驻。MCP 连接和工具初始化仍沿用现有流程；本次延迟的是向模型发送说明，不是 MCP 连接。
- 自动触发 Skill 按内容 hash 去重，只在完整内容仍存在于保留的消息中时复用。内容变化、截断或压缩移除后重新注入。匹配规则仍在本地运行，不再重复写入 Skill 工具目录。

## Prompt 与缓存

默认行为规则已经去重，保留授权、Git 安全、验证、项目约束和权限拒绝处理。PromptProfile 的 `None`/空字符串/自定义文本语义保持不变。环境使用实际 Git 状态和模型 ID，不再附带固定 Claude 产品宣传或猜测知识截止日期。

静态边界用 `SystemPrompt.static_count` 传递，不再把魔法标记发给模型。直接 Anthropic 端点在最后一个静态块上附加 `cache_control`；自定义网关、Bedrock、Vertex 默认不自动添加，支持时可显式配置：

```json
{ "api": { "prompt_caching": "enabled" } }
```

`disabled` 关闭这项显式 Anthropic 标记；`auto` 为默认。这个设置不关闭 OpenAI 服务端自动缓存，现有 OpenAI 缓存配置未更改。

缓存依赖一致的前缀；普通工具 schema 集合增加时仍可能使已有缓存失效，稳定排序不能消除这个影响。[Anthropic 缓存文档](https://platform.claude.com/docs/en/build-with-claude/prompt-caching)。本次采用跨 provider 的普通函数工具协议，未启用 provider 原生 `defer_loading` 或假定兼容网关支持原生工具搜索。

## 观测与验证

CLI `/status`、Desktop 上下文面板和 VS Code `/status` 显示上次请求的已加载/可用工具数，以及 system、tools、目录的本地估算。目录已经计入 system，不能再次相加。服务端计费/缓存统计与这些本地分项估算分开。

运行 `python scripts/prompt_budget.py` 可复现默认输入的离线 `o200k_base` 文本计数。脚本不读取凭证、项目指令或历史，不调用模型；不含 provider 内部包装开销。

本次在相同 `/workspace`、GPT 模型标识、默认工具和未开启原生图片生成的环境下测得：

| 组成 | 修改前 | 精简 prompt + discovery |
| --- | ---: | ---: |
| System（不含目录） | 4,361 | 1,015 |
| 工具目录 | 0 | 159 |
| 工具 schema | 7,850 | 2,964 |
| 合计 | 12,211 | 4,138 |
| 首轮工具数 | 46 | 12 |

当前新版 `eager` 合计为 9,007 tokens；它包含新增 ToolSearch。注册表共 48 项时，47 项在该模型能力条件下可用，ImageGenerate 不可用；安装 Skill/MCP/额外工具或启用原生图片生成会改变结果。

回归覆盖 schema 下一轮生效、直接越过发现的调用被拒绝、原权限生效、Plan/模型可用性过滤、会话隔离与恢复/fork、Skill 去重、缓存序列化和预算上限。离线缩减不能证明真实模型任务质量、延迟或账单收益；还需真实任务的 eager/discovery A/B 来评估这些指标。

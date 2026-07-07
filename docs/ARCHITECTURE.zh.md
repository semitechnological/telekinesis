# Telekinesis 架构

## 目标

- 一个统一的 Agent 工作区，可在手机、桌面 GUI、终端 TUI 之间无缝切换。
- 多个 Agent 可以并行运行，每个 Agent 可以使用不同的模型和配置。
- 跨设备控制：登录一台设备后，可以在其他设备上继续控制同一个工作区。
- 通过自建 provider gateway 支持商业化。

## 分层

```
┌─────────────────────────────────────────────┐
│  UI 层 (Crepuscularity)                       │
│  - 手机 / 桌面 GUI / TUI / Web / 浏览器扩展   │
│  - 统一 .crepus 语法                          │
└──────────────────┬──────────────────────────┘
                   │ View IR / JSON
┌──────────────────▼──────────────────────────┐
│  控制层 (Zig)                                 │
│  - 会话管理 (session.zig)                     │
│  - Agent 循环 (agent.zig)                     │
│  - 插件 / extension (plugin.zig)              │
│  - ACP 主机 (acp.zig)                         │
└──────────────────┬──────────────────────────┘
                   │
┌──────────────────▼──────────────────────────┐
│  网络层 (Zig)                                 │
│  - 信令服务 (signaling)                       │
│  - P2P 连接 (QUIC/WebRTC)                     │
│  - 跨设备同步                               │
└──────────────────┬──────────────────────────┘
                   │
┌──────────────────▼──────────────────────────┐
│  Provider 层 (Zig)                          │
│  - 统一 LLM API (provider.zig)              │
│  - 自建 gateway / 计费 / 路由               │
│  - 支持 OpenAI、Anthropic、Gemini、本地模型等 │
└─────────────────────────────────────────────┘
```

## 核心模块

### agent.zig

Agent 循环受 pi 启发：
- `agent_start` → `turn_start` → `message_start/end` → `tool_execution_*` → `turn_end` → `agent_end`
- 支持 steering（打断）和 follow-up（后续消息）。
- 支持并行 / 顺序工具执行。
- 插件可以在事件流中拦截、修改或注入。

### session.zig

会话持久化：
- JSONL 或 SQLite 存储会话树。
- 支持 fork / clone / branch / compact。
- 跨设备同步通过 net 层。

### plugin.zig

插件系统受 pi 和 zero 启发：
- 支持两种插件：Zig 插件（动态加载）和 TypeScript 插件（通过 Bun/Node 子进程）。
- 插件可以注册工具、命令、事件处理器。
- 未来支持 skills（Markdown 能力包）和 hooks。

### acp.zig

ACP 主机受 Zed 启发：
- 实现 Agent Client Protocol 主机端。
- 外部 Agent 作为独立进程运行，通过 JSON-RPC 通信。
- 转发 MCP 服务器和工具。

### net.zig

跨设备网络：
- 自建的 lightweight 信令服务（HTTP/QUIC）。
- 设备发现后优先 P2P 连接（QUIC over UDP，失败回退 TURN）。
- 不依赖 Tailscale，但可保留 Tailscale 集成作为备选。

### provider.zig

Provider 网关：
- 统一的多 provider API：OpenAI、Anthropic、Google、OpenRouter、本地模型等。
- 自建 gateway 支持计费、限流、缓存、路由。
- 配置受 opencode/crush 的 provider 配置启发。

### lsp.zig

LSP 集成：
- 管理每个工作区的 LSP 客户端。
- Agent 可以通过 LSP 获取符号、类型、诊断等信息。

## UI 层

使用 Crepuscularity 的 `.crepus` DSL：
- `ui/app.crepus` — 主应用布局。
- `ui/shell.crepus` — 终端 / Agent 面板。
- 同一个模板可以编译到 GPUI（桌面）、Ratatui（TUI）、HTML（Web）、SwiftUI（iOS）、Jetpack Compose（Android）。

## 第一阶段

1. 搭建 Zig 项目骨架，跑通 `zig build run`。
2. 实现一个最简单的 Agent 循环（单 provider、单工具）。
3. 用 Crepuscularity 渲染一个最简单的 TUI 界面。
4. 实现 provider gateway 的最小 API（转发 OpenAI 兼容请求）。
5. 写测试。

## 待定

- 插件系统：Zig 动态加载 vs 子进程通信。
- 持久化：JSONL vs SQLite。
- 网络：QUIC vs WebRTC。
- LSP 集成范围。

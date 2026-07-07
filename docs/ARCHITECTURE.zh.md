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

会话持久化：**SQLite**（通过 `zig-sqlite` 绑定），每个工作区一个 `.telekinesis/session.db`。
- 表结构：`sessions`（元数据）、`messages`（内容 + 角色 + 父指针，用于会话树）、`tool_calls`、`entries`（扩展写入的非对话状态，对齐 pi 的 `appendEntry`）。
- 会话树通过 `messages.parent_id` 实现，天然支持 fork / clone / branch；compact 通过写入一条汇总 message 并重新挂接子树实现。
- 决策依据：
  - **拒绝 SurrealDB**：没有原生 Zig 绑定，只能作为独立服务运行，违背「单二进制、本地优先」的目标。
  - **拒绝 RocksDB**：`zig-rocksdb`/`zrocks` 等绑定存在，但 RocksDB 是 C++ 库，跨工具链链接脆弱（libstdc++ vs libc++ ABI），且当前规模下不需要 LSM 树的写入吞吐优势。
  - SQLite 绑定成熟（`zig-sqlite`），单文件、可直接用 `sqlite3` CLI 检查/调试，符合 MVP 优先级。
  - 未来如果需要跨会话全文检索或更大规模的事件流存储，可以在 SQLite 之上加 FTS5，或引入 RocksDB 作为可插拔的第二后端（`session.zig` 用接口/vtable 隔离存储实现，避免耦合）。
- 跨设备同步通过 net 层（增量同步 `messages`/`entries` 的变更日志，而不是同步整个 DB 文件）。

### plugin.zig

插件系统采用 **pi 兼容**的子进程模型，目标是直接复用 pi 生态的 TypeScript 扩展：

- **扩展（extensions）**：TypeScript/JavaScript 模块，通过 **Bun 子进程**加载（pi 用 jiti 在进程内加载 TS，我们用 Bun 作为外部 TS 运行时达到同等效果）。
  - 通信协议：**JSONL over stdio**，与 pi 的 RPC 模式一致（`docs/rpc.md`）——请求是 `{"id":..., "type":..., ...}`，响应是 `{"id":..., "type":"response", "command":..., "success":...}`，事件以 JSON 行流式输出到 stdout。
  - Host（Zig）侧维持一个 `Host` vtable，把 pi 风格的能力（`registerTool` / `registerCommand` / `on(event, cb)` / `sendMessage` / `appendEntry` / `setModel` 等）翻译成发往子进程的 JSON-RPC 调用；子进程侧用一个薄 shim 把这些调用映射回 pi 的 `ExtensionAPI`。
  - 加载约定与 pi 一致：项目级 `.telekinesis/extensions/*.ts` 或 `.telekinesis/extensions/*/index.ts`，全局 `~/.telekinesis/extensions/`；子目录带 `package.json` 且 `pi.extensions` 字段时按 manifest 加载。
  - 崩溃隔离：每个扩展独立子进程，崩溃不影响 host；host 监控子进程退出并按策略重启或标记失败。
- **技能（skills）**：与扩展完全独立的机制，纯 Markdown 能力包（`SKILL.md`，YAML frontmatter + 正文），是被动注入到 system prompt 的知识，不能注册工具或拦截事件。目录约定与 pi 对齐：项目级 `.telekinesis/skills/`，全局 `~/.telekinesis/skills/`；以 `<available_skills>` XML 标签注入 system prompt。
- **事件生命周期**（在 `agent.zig` 的 `Event` 基础上扩展，命名与 pi 完全对齐，便于直接复用 pi 扩展代码）：
  ```
  before_agent_start（可修改 messages / system prompt）
    → agent_start
      → turn_start
        → message_start → message_update* → message_end
        → tool_call（可阻止调用）→ tool_execution_start → tool_execution_update* → tool_execution_end → tool_result
      → turn_end
  → agent_end
  ```
- 选择子进程 + Bun 而不是原生 Zig 动态库的理由：**pi 兼容是硬性目标**——pi 扩展是 TypeScript，只能在外部 TS 运行时里跑；进程内 Zig 动态库无法执行 TS 代码。子进程模型还带来天然崩溃隔离。`acp.zig` 的 ACP 协议用于「外部 Agent 进程」，`plugin.zig` 的 JSON-RPC 用于「扩展子进程」，两者职责不重叠。

### acp.zig

ACP 主机受 Zed 启发：
- 实现 Agent Client Protocol 主机端。
- 外部 Agent 作为独立进程运行，通过 JSON-RPC 通信。
- 转发 MCP 服务器和工具。

### net.zig

跨设备网络：**QUIC**，而不是 WebRTC。
- 决策依据：
  - 已有可用的纯 Zig QUIC 实现（如 `quic-zig`、`zquic`），覆盖 RFC 9000/9001/9002，避免通过 FFI 绑定 `libdatachannel`（WebRTC 的事实标准库，C++，构建复杂、体积大）。
  - QUIC 原生支持连接迁移（网络切换时，如 WiFi ↔ 蜂窝，连接不断），这正是「多设备无缝切换」场景需要的。
  - 部分纯 Zig QUIC 实现同时提供 HTTP/3 和 WebTransport，未来 Web UI（浏览器 Surface）可以直接用 WebTransport 复用同一套传输层，不需要再实现 WebRTC data channel。
  - 劣势（可接受）：浏览器原生不能直接发起裸 QUIC 连接，只能通过 WebTransport（需要服务端证书），已在设计中考虑。
- 架构：
  - 自建 lightweight 信令服务（HTTP），负责设备注册、会话邀请、候选地址交换。
  - 设备发现后优先建立 P2P QUIC 连接（打洞失败则回退到自建的 relay：一个转发 QUIC 流量的中继节点，效果类似 TURN，但走同一套 QUIC 协议栈，不需要额外实现 TURN）。
  - 不依赖 Tailscale；不排除未来支持「导入 Tailscale/WireGuard 网络」作为可选传输后端。

### provider.zig

Provider 网关：
- 统一的多 provider API：OpenAI、Anthropic、Google、OpenRouter、本地模型等。
- 自建 gateway 支持计费、限流、缓存、路由。
- 配置受 opencode/crush 的 provider 配置启发。

### lsp.zig

LSP 集成（**多语言**）：
- 管理每个工作区的多个 LSP 客户端，按语言 ID 路由请求（`zig`/`rust`/`go`/`typescript`/`python`/...）。
- 每个 LSP 客户端是一个独立子进程，通过 stdio 与 LSP server 通信（标准 JSON-RPC 2.0 over stdio）。
- Agent 可以通过统一 API 获取诊断、go-to-definition、hover、引用、符号等，无论目标语言是什么。
- 语言 → server 命令的映射通过配置文件提供（默认带常见语言的合理默认值，如 `zig → zls`、`rust → rust-analyzer`、`typescript → typescript-language-server`），用户可覆盖。

## UI 层

使用 Crepuscularity 的 `.crepus` DSL：
- `ui/app.crepus` — 主应用布局。
- `ui/shell.crepus` — 终端 / Agent 面板。
- 同一个模板可以编译到 GPUI（桌面）、Ratatui（TUI）、HTML（Web）、SwiftUI（iOS）、Jetpack Compose（Android）。

## 路线图

阶段 0（骨架）已完成：`zig build run` 可跑通，所有模块有 stub 和最小测试。

下一阶段是多条线并行推进（模块边界清晰，互不阻塞）：

1. **Provider 网关**（`provider.zig`）
   - 真实 HTTP client，支持 OpenAI 兼容的 chat completion（含流式 SSE）。
   - 至少接入一个真实 provider（OpenAI 或 Anthropic）用于联调。
2. **Agent 循环**（`agent.zig`）
   - 接入 provider.zig，实现单轮真实对话（prompt → 流式回复）。
   - 补齐 `before_agent_start` / `tool_call` / `tool_result` 等事件，对齐 plugin.zig 的生命周期设计。
   - 至少实现一个内置工具（如 `read_file`）跑通 tool_execution_* 全流程。
3. **会话持久化**（`session.zig`）
   - 引入 `zig-sqlite` 依赖，落地 `sessions`/`messages`/`tool_calls`/`entries` schema。
   - 支持 resume（重启后恢复会话）与 fork。
4. **插件系统**（`plugin.zig`）
   - 实现 Bun 子进程 + JSONL/JSON-RPC over stdio 的 host 端，对齐 pi 的 RPC 模式。
   - 实现 `Host` vtable：`registerTool` / `registerCommand` / `on(event, cb)` / `sendMessage` / `appendEntry` / `setModel` 等翻译成 JSON-RPC 调用。
   - 加载器：扫描 `.telekinesis/extensions/` 和 `~/.telekinesis/extensions/`，按 pi 约定发现 `.ts`/`index.ts`/`package.json` manifest。
   - 提供一个最小 shim（TypeScript）让 pi 扩展能在 Bun + 我们的 RPC 下运行，验证闭环。
   - 技能（skills）加载器：扫描 `SKILL.md`，以 `<available_skills>` XML 注入 system prompt。
5. **网络层**（`net.zig`）
   - 引入纯 Zig QUIC 依赖，先实现单机内 client/server QUIC 握手 + 一个双向 stream 的最小 demo。
   - 信令服务的最小 HTTP API（注册设备、交换候选地址）。
   - P2P 打洞和 relay 回退放在这之后，作为独立子任务。
6. **ACP 主机**（`acp.zig`）
   - 实现 ACP JSON-RPC 的最小握手 + 一次 turn，宿主一个外部 Agent 子进程（可先用 Zed 的参考实现对拍）。
7. **UI**（`ui/*.crepus`）
   - 优先跑通 TUI 目标（Ratatui），展示 agent.zig 的事件流。
   - 桌面 GUI（GPUI）和 Web 目标在 TUI 验证事件契约后再接入。
8. **LSP 集成**（`lsp.zig`）
   - 多语言 LSP 客户端管理器：按语言 ID 路由，每语言一个子进程，标准 JSON-RPC 2.0 over stdio。
   - 先实现诊断 + go-to-definition + hover 三个能力，跨语言统一 API。
   - 语言 → server 命令的默认映射 + 用户配置覆盖。

以上各项可以拆成独立 issue 并行开工；模块间的耦合点是 `agent.zig` 的 `Event` 类型和 `session.zig` 的存储接口，改动这两者需要先同步。

## 已解决的决策

- 持久化：SQLite（`zig-sqlite`），理由见 `session.zig` 一节。
- 插件系统：**pi 兼容**——Bun 子进程 + JSONL/JSON-RPC over stdio，直接复用 pi 的 TypeScript 扩展生态，理由见 `plugin.zig` 一节。
- 网络：QUIC（纯 Zig 实现），理由见 `net.zig` 一节。
- LSP 范围：**多语言**，按语言 ID 路由到独立 LSP 子进程，统一 API，理由见 `lsp.zig` 一节。

## 待定

- 插件 shim 的具体 API 覆盖范围（哪些 pi `ExtensionAPI` 方法先支持，哪些后补）。
- 插件是否需要 WASM 沙箱隔离作为第二格式（v1 不做，仅 Bun 子进程）。

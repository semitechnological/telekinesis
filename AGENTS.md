# Telekinesis 开发指南

## 项目概述

Telekinesis 是一个多表面（multi-surface）的 Agent 工作区：你可以通过手机、桌面 GUI 或终端 TUI 控制编码 Agent，并在不同设备之间切换而不会丢失上下文。

我们的目标是：
- **UX 像 t3code 一样极简、快速**
- **Agent 系统像 pi 一样可扩展（skills + extensions）**
- **支持 ACP（Agent Client Protocol），与 Zed 的外部 Agent 兼容**
- **跨设备使用自己的信令 + P2P（QUIC/WebRTC）**
- **通过自建的 provider gateway 实现商业化**

## 技术栈

- **UI 层：** Crepuscularity（`.crepus` DSL，Rust/GPUI 运行时）
  - 参考仓库：`../crepuscularity`
  - Crepuscularity 已有 Zig 插件桥：`../crepuscularity/plugins/zig/crepuscularity.zig`
- **后端 / Agent 运行时：** Zig
  - 位于 `src/`
  - 包含 agent 循环、网络、provider 网关、ACP 主机、LSP 集成
  - 持久化：SQLite（`zig-sqlite`），单文件 `.telekinesis/session.db`
  - 网络：纯 Zig QUIC 实现（如 `quic-zig`/`zquic`），不用 WebRTC
  - 插件：**pi 兼容**——Bun 子进程 + JSONL/JSON-RPC over stdio，直接复用 pi 的 TypeScript 扩展；skills 是独立的被动 Markdown 机制（`SKILL.md`）
  - LSP：多语言，按语言 ID 路由到独立 LSP 子进程（标准 JSON-RPC 2.0 over stdio）
- **参考实现：**
  - `references/t3code` — UX 与多表面架构
  - `references/pi` — Agent 循环、skills、extensions
  - `references/zed` — ACP server 参考
  - `references/anomalyco-opencode` — provider 网关与全栈 Agent 工具
  - `references/crush` — LSP、MCP、skills、hooks
  - `references/zero` — MCP client/server、skills、hooks、plugins、specialists

## 目录结构

```
telekinesis/
  src/
    main.zig      — 入口
    agent.zig     — Agent 循环、事件、工具注册
    config.zig    — 配置加载 (config.json + env vars)
    tools.zig     — 内置工具 (read_file, write_file, list_dir, run_command)
    net.zig       — P2P / 信令 / QUIC
    provider.zig  — LLM provider 抽象与 gateway
    acp.zig       — ACP server 主机
    session.zig   — 会话持久化 (JSONL)
    lsp.zig       — LSP 客户端管理
    plugin.zig    — 插件 / extension 接口
    ipc.zig       — JSON-RPC IPC server (Unix socket)
  ui/             — Crepuscularity 模板 + TUI/GUI
  docs/           — 中文架构文档
  references/     — 子模块引用
  build.zig
  crepus.toml
```

## 构建命令

```bash
zig build              # 编译 Zig 核心
zig build run          # 运行 Zig 核心
zig build test         # 测试 Zig 核心
zig build run -- serve # 启动 IPC 服务器 (Unix socket: ~/.telekinesis/telekinesis.sock)

# TUI (Rust, crepuscularity-tui + ratatui)
cd ui/tui && cargo build
cd ui/tui && cargo run   # 需要 `telekinesis serve` 先启动

# GUI (Rust, crepuscularity-gpui)
cd ui/gui && cargo build
cd ui/gui && cargo run   # 需要 `telekinesis serve` 先启动
```

## 配置

配置文件位于 `~/.telekinesis/config.json`，支持环境变量覆盖：

```json
{
  "default_provider": "openai",
  "default_model": "gpt-4o",
  "api_keys": {
    "openai": "sk-...",
    "anthropic": "sk-ant-..."
  },
  "system_prompt": "You are a helpful coding assistant."
}
```

环境变量：`OPENAI_API_KEY`、`ANTHROPIC_API_KEY`、`TELEKINESIS_PROVIDER`、`TELEKINESIS_MODEL`。

## IPC 协议

JSON-RPC over Unix socket。支持的方法：

- `state` — 获取当前状态 (model, messages, tools, plugins)
- `prompt` — 发送用户消息
- `set_model` — 切换模型
- `tools` — 列出可用工具
- `call_tool` — 手动调用工具
- `messages` — 获取消息历史
- `session_list` — 列出已保存会话
- `session_create` — 创建新会话
- `session_load` — 加载会话
- `session_save` — 保存当前会话
- `session_clear` — 清空当前会话
- `ping` — 健康检查

事件通过 JSON-RPC notification 推送（`method: "event"`）。

## 代码规范

- Zig 代码使用显式 allocator，不依赖全局 allocator。
- 错误处理使用显式 error set，不滥用 `anyerror`。
- 优先使用 `const` 和 `switch`，避免不必要的 `var`。
- 每个模块顶部定义 `const log = std.log.scoped(.模块名)`。
- 测试放在 `src/..._test.zig` 或源文件底部的 `test {}` 块中。
- Crepuscularity 模板使用 2 空格缩进，UnoCSS 风格类名。
- 新增 Zig 依赖用 `zig fetch --save`，优先选择发布至少一周、有多个使用者的版本；避免拉入需要 FFI 绑定 C++ 库的依赖（参考 net.zig 拒绝 WebRTC/libdatachannel 的理由）。
- 插件（extensions）走 `plugin.zig` 的 pi 兼容协议：Bun 子进程 + JSONL/JSON-RPC over stdio；不要为插件另起一套进程内动态库协议——外部 Agent 进程集成走 `acp.zig` 的 ACP 协议，扩展子进程走 `plugin.zig` 的 JSON-RPC，两者职责不重叠。

## 协作风格

- 保持简洁直接，不添加无关的礼貌性文字。
- 提交信息使用英文，格式为 `type(scope): summary`（例如 `feat(agent): add session tree`）。
- 修改前先读取完整文件，避免基于搜索片段做大幅改动。
- 不添加未请求的功能；遇到不确定的决策时先停下来询问。

## 安全

- 不要硬编码 API key、secret 或凭证。
- 不提交 `.env` 或本地配置文件。
- provider 网关只转发请求，不存储用户对话内容。

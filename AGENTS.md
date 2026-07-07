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
  - 插件：进程内原生 Zig 动态库（`std.DynLib` + C ABI `register(host)`），skills 是独立的被动 Markdown 机制（`SKILL.md`）
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
    agent.zig     — Agent 循环与事件
    net.zig       — P2P / 信令 / QUIC
    provider.zig  — LLM provider 抽象与 gateway
    acp.zig       — ACP server 主机
    session.zig   — 会话持久化
    lsp.zig       — LSP 客户端管理
    plugin.zig    — 插件 / extension 接口
  ui/             — Crepuscularity 模板
  docs/           — 中文架构文档
  references/     — 子模块引用
  build.zig
  crepus.toml
```

## 构建命令

```bash
zig build              # 编译
zig build run          # 运行
zig build test         # 测试
```

## 代码规范

- Zig 代码使用显式 allocator，不依赖全局 allocator。
- 错误处理使用显式 error set，不滥用 `anyerror`。
- 优先使用 `const` 和 `switch`，避免不必要的 `var`。
- 每个模块顶部定义 `const log = std.log.scoped(.模块名)`。
- 测试放在 `src/..._test.zig` 或源文件底部的 `test {}` 块中。
- Crepuscularity 模板使用 2 空格缩进，UnoCSS 风格类名。
- 新增 Zig 依赖用 `zig fetch --save`，优先选择发布至少一周、有多个使用者的版本；避免拉入需要 FFI 绑定 C++ 库的依赖（参考 net.zig 拒绝 WebRTC/libdatachannel 的理由）。
- 插件（extensions）遵循 `plugin.zig` 中定义的 `Host` vtable 约定；不要为插件另起一套子进程 RPC 协议——外部进程集成走 `acp.zig` 的 ACP 协议。

## 协作风格

- 保持简洁直接，不添加无关的礼貌性文字。
- 提交信息使用英文，格式为 `type(scope): summary`（例如 `feat(agent): add session tree`）。
- 修改前先读取完整文件，避免基于搜索片段做大幅改动。
- 不添加未请求的功能；遇到不确定的决策时先停下来询问。

## 安全

- 不要硬编码 API key、secret 或凭证。
- 不提交 `.env` 或本地配置文件。
- provider 网关只转发请求，不存储用户对话内容。

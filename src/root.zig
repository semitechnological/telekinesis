const std = @import("std");

/// Product surface: multi-device UI + P2P. Agent harness lives in rotary.
pub const rotary = @import("rotary");
pub const httpx = @import("httpx");
pub const net = @import("net.zig");

pub const version = "0.2.0";

pub fn printBanner(writer: *std.Io.Writer) !void {
    try writer.print("Telekinesis {s} (rotary {s})\n", .{ version, rotary.version });
}

// Re-export rotary harness surface so existing telekinesis imports keep working.
pub const agent = rotary.agent;
pub const provider = rotary.provider;
pub const session = rotary.session;
pub const lsp = rotary.lsp;
pub const plugin = rotary.plugin;
pub const acp = rotary.acp;
pub const ipc = rotary.ipc;
pub const config = rotary.config;
pub const tools = rotary.tools;
pub const db = rotary.db;
pub const permissions = rotary.permissions;
pub const context = rotary.context;
pub const slash = rotary.slash;
pub const hooks = rotary.hooks;
pub const extract = rotary.extract;
pub const ranking = rotary.ranking;
pub const guardrails = rotary.guardrails;

pub const Event = rotary.Event;
pub const Agent = rotary.Agent;
pub const Provider = rotary.Provider;
pub const Session = rotary.Session;
pub const ToolRegistry = rotary.ToolRegistry;
pub const ToolDefinition = rotary.ToolDefinition;
pub const ToolCall = rotary.ToolCall;
pub const ToolResult = rotary.ToolResult;
pub const ProviderClient = rotary.ProviderClient;
pub const ProviderRegistry = rotary.ProviderRegistry;
pub const PluginToolBridge = rotary.PluginToolBridge;
pub const PluginAction = rotary.PluginAction;
pub const PluginActionHandler = rotary.PluginActionHandler;
pub const PluginUiRequest = rotary.PluginUiRequest;
pub const PluginUiResponse = rotary.PluginUiResponse;
pub const PluginUiHandler = rotary.PluginUiHandler;
pub const Policy = rotary.Policy;
pub const PermissionMode = rotary.PermissionMode;

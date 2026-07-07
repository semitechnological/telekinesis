const std = @import("std");

pub const agent = @import("agent.zig");
pub const net = @import("net.zig");
pub const provider = @import("provider.zig");
pub const session = @import("session.zig");
pub const lsp = @import("lsp.zig");
pub const plugin = @import("plugin.zig");
pub const acp = @import("acp.zig");

pub const version = "0.1.0";

pub fn printBanner(writer: *std.Io.Writer) !void {
    try writer.print("Telekinesis {s}\n", .{version});
}

pub const Event = agent.Event;
pub const Agent = agent.Agent;
pub const Provider = provider.Provider;
pub const Session = session.Session;
pub const ToolRegistry = agent.ToolRegistry;
pub const ToolDefinition = agent.ToolDefinition;
pub const ToolCall = agent.ToolCall;
pub const ToolResult = agent.ToolResult;
pub const ProviderClient = provider.Client;
pub const ProviderRegistry = provider.Registry;
pub const PluginToolBridge = plugin.PluginToolBridge;

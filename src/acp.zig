const std = @import("std");

const log = std.log.scoped(.acp);

pub const AgentId = []const u8;

pub const Host = struct {
    allocator: std.mem.Allocator,

    pub fn init(allocator: std.mem.Allocator) Host {
        return .{ .allocator = allocator };
    }

    pub fn spawn(self: *Host, agent_id: AgentId, command: []const u8) !void {
        log.info("spawning ACP agent {s}: {s}", .{ agent_id, command });
        _ = self;
    }

    pub fn forwardTool(self: *Host, agent_id: AgentId, tool_name: []const u8) !void {
        log.info("forwarding tool {s} to agent {s}", .{ tool_name, agent_id });
        _ = self;
    }
};

test "acp host can spawn agent" {
    const gpa = std.testing.allocator;
    var host = Host.init(gpa);
    try host.spawn("codex", "npx @openai/codex");
}

const std = @import("std");
const agent = @import("agent.zig");

const log = std.log.scoped(.session);

pub const SessionId = []const u8;

pub const Session = struct {
    id: SessionId,
    name: []const u8,
    messages: std.ArrayList(agent.Message),

    pub fn init(allocator: std.mem.Allocator, id: SessionId, name: []const u8) !Session {
        return .{
            .id = try allocator.dupe(u8, id),
            .name = try allocator.dupe(u8, name),
            .messages = std.ArrayList(agent.Message).empty,
        };
    }

    pub fn deinit(self: *Session, allocator: std.mem.Allocator) void {
        allocator.free(self.id);
        allocator.free(self.name);
        for (self.messages.items) |message| {
            allocator.free(message.content);
        }
        self.messages.deinit(allocator);
    }

    pub fn append(self: *Session, allocator: std.mem.Allocator, message: agent.Message) !void {
        const owned = try allocator.dupe(u8, message.content);
        errdefer allocator.free(owned);
        try self.messages.append(allocator, .{ .role = message.role, .content = owned });
        log.info("session {s}: appended {s} message", .{ self.id, @tagName(message.role) });
    }
};

test "session holds messages" {
    const gpa = std.testing.allocator;
    var session = try Session.init(gpa, "abc", "test");
    defer session.deinit(gpa);

    try session.append(gpa, .{ .role = .user, .content = "hello" });
    try std.testing.expectEqual(@as(usize, 1), session.messages.items.len);
}

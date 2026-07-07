const std = @import("std");

const log = std.log.scoped(.agent);

pub const Event = union(enum) {
    agent_start,
    turn_start,
    message_start: Message,
    message_update: []const u8,
    message_end: Message,
    tool_execution_start: ToolCall,
    tool_execution_end: ToolResult,
    turn_end,
    agent_end,
};

pub const Message = struct {
    role: Role,
    content: []const u8,

    pub const Role = enum {
        user,
        assistant,
        system,
        tool,
    };
};

pub const ToolCall = struct {
    id: []const u8,
    name: []const u8,
    arguments: []const u8,
};

pub const ToolResult = struct {
    id: []const u8,
    content: []const u8,
    is_error: bool,
};

pub const Subscriber = *const fn (?*anyopaque, Event) void;

pub const Agent = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    subscriber: ?Subscriber,
    subscriber_ctx: ?*anyopaque,
    messages: std.ArrayList(Message),

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Agent {
        return .{
            .allocator = allocator,
            .io = io,
            .subscriber = null,
            .subscriber_ctx = null,
            .messages = std.ArrayList(Message).empty,
        };
    }

    pub fn deinit(self: *Agent) void {
        for (self.messages.items) |message| {
            self.allocator.free(message.content);
        }
        self.messages.deinit(self.allocator);
    }

    pub fn subscribe(self: *Agent, ctx: ?*anyopaque, callback: Subscriber) void {
        self.subscriber_ctx = ctx;
        self.subscriber = callback;
    }

    pub fn prompt(self: *Agent, text: []const u8) !void {
        const owned = try self.allocator.dupe(u8, text);
        errdefer self.allocator.free(owned);
        try self.messages.append(self.allocator, .{ .role = .user, .content = owned });

        self.emit(.agent_start);
        self.emit(.{ .message_start = .{ .role = .user, .content = owned } });
        self.emit(.{ .message_end = .{ .role = .user, .content = owned } });
        self.emit(.turn_end);
        self.emit(.agent_end);
    }

    fn emit(self: *Agent, event: Event) void {
        if (self.subscriber) |callback| {
            callback(self.subscriber_ctx, event);
        } else {
            log.info("{}", .{event});
        }
    }
};

test "agent prompt emits events" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var agent = Agent.init(gpa, io);
    defer agent.deinit();

    var count: usize = 0;
    agent.subscribe(&count, struct {
        fn cb(ctx: ?*anyopaque, event: Event) void {
            _ = event;
            const ptr: *usize = @ptrCast(@alignCast(ctx.?));
            ptr.* += 1;
        }
    }.cb);
    try agent.prompt("hi");

    try std.testing.expectEqual(@as(usize, 5), count);
}

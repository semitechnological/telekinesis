const std = @import("std");
const agent = @import("agent.zig");
const provider = @import("provider.zig");
const plugin = @import("plugin.zig");

const log = std.log.scoped(.ipc);

pub const RpcError = error{
    MethodNotFound,
    InvalidParams,
    InternalError,
};

pub const Server = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    socket_path: []const u8,
    agent_instance: ?*agent.Agent = null,
    tool_registry: ?*agent.ToolRegistry = null,
    plugin_registry: ?*plugin.Registry = null,
    arena: std.heap.ArenaAllocator,
    event_subscriber_ctx: EventSubscriberCtx,

    const EventSubscriberCtx = struct {
        server: *Server,
        current_writer: ?*std.Io.Writer = null,
    };

    pub fn init(allocator: std.mem.Allocator, io: std.Io, socket_path: []const u8) Server {
        return .{
            .allocator = allocator,
            .io = io,
            .socket_path = socket_path,
            .arena = std.heap.ArenaAllocator.init(allocator),
            .event_subscriber_ctx = undefined,
        };
    }

    pub fn deinit(self: *Server) void {
        self.arena.deinit();
    }

    pub fn attachAgent(self: *Server, a: *agent.Agent) void {
        self.agent_instance = a;
        self.event_subscriber_ctx = .{ .server = self, .current_writer = null };
        a.subscribe(&self.event_subscriber_ctx, eventCallback) catch {};
    }

    pub fn attachTools(self: *Server, tr: *agent.ToolRegistry) void {
        self.tool_registry = tr;
    }

    pub fn attachPlugins(self: *Server, pr: *plugin.Registry) void {
        self.plugin_registry = pr;
    }

    fn eventCallback(ctx: ?*anyopaque, event: agent.Event) void {
        const esc: *EventSubscriberCtx = @ptrCast(@alignCast(ctx.?));
        if (esc.current_writer) |w| {
            pushEvent(w, event) catch {};
        }
    }

    pub fn run(self: *Server) !void {
        _ = self.arena.reset(.retain_capacity);
        const arena = self.arena.allocator();

        std.Io.Dir.deleteFileAbsolute(self.io, self.socket_path) catch {};

        const unix_addr = try std.Io.net.UnixAddress.init(self.socket_path);
        var server = try std.Io.net.UnixAddress.listen(&unix_addr, self.io, .{});
        defer server.deinit(self.io);

        log.info("IPC server listening on {s}", .{self.socket_path});

        while (true) {
            const stream = server.accept(self.io) catch |err| {
                log.err("accept failed: {}", .{err});
                continue;
            };

            self.handleConnection(arena, stream) catch |err| {
                log.err("connection error: {}", .{err});
            };
            stream.close(self.io);
            _ = self.arena.reset(.retain_capacity);
        }
    }

    fn handleConnection(self: *Server, arena: std.mem.Allocator, stream: std.Io.net.Stream) !void {
        var read_buf: [16384]u8 = undefined;
        var reader = std.Io.net.Stream.Reader.init(stream, self.io, &read_buf);
        const r = &reader.interface;

        var write_buf: [16384]u8 = undefined;
        var writer = std.Io.net.Stream.Writer.init(stream, self.io, &write_buf);
        const w = &writer.interface;

        while (true) {
            const line = r.takeDelimiter('\n') catch |err| {
                if (err == error.EndOfStream) return;
                return err;
            };
            if (line == null) return;

            self.event_subscriber_ctx.current_writer = w;
            const response = try self.handleRequest(arena, line.?);
            try w.print("{s}\n", .{response});
            try w.flush();
            self.event_subscriber_ctx.current_writer = null;
        }
    }

    fn handleRequest(self: *Server, arena: std.mem.Allocator, line: []const u8) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;

        const parsed = std.json.parseFromSlice(Request, arena, line, .{
            .ignore_unknown_fields = true,
        }) catch {
            try writeError(out, null, -32700, "Parse error");
            return buf.written();
        };

        const result = self.dispatch(arena, parsed.value.method, parsed.value.params) catch |err| {
            const msg = @errorName(err);
            try writeError(out, parsed.value.id, -32603, msg);
            return buf.written();
        };

        try writeResult(out, parsed.value.id, result);
        return buf.written();
    }

    fn dispatch(self: *Server, arena: std.mem.Allocator, method: []const u8, params: ?std.json.Value) ![]const u8 {
        if (std.mem.eql(u8, method, "state")) return self.getState(arena);
        if (std.mem.eql(u8, method, "prompt")) return self.sendPrompt(arena, params);
        if (std.mem.eql(u8, method, "set_model")) return self.setModel(arena, params);
        if (std.mem.eql(u8, method, "tools")) return self.getTools(arena);
        if (std.mem.eql(u8, method, "plugins")) return self.getPlugins(arena);
        if (std.mem.eql(u8, method, "messages")) return self.getMessages(arena);
        if (std.mem.eql(u8, method, "call_tool")) return self.callTool(arena, params);
        if (std.mem.eql(u8, method, "ping")) return self.ping(arena);
        return error.MethodNotFound;
    }

    fn ping(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        _ = self;
        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"pong\":true}}", .{});
        return buf.written();
    }

    fn getState(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("{{\"model\":\"{s}\",\"messages\":{d},\"tools\":{d}", .{
            if (self.agent_instance) |a| a.model else "none",
            if (self.agent_instance) |a| a.messages.items.len else 0,
            if (self.tool_registry) |tr| tr.tools.count() else 0,
        });
        if (self.plugin_registry) |pr| {
            try out.print(",\"plugins\":{d}", .{pr.plugins.items.len});
        } else {
            try out.print(",\"plugins\":0", .{});
        }
        try out.print("}}", .{});
        return buf.written();
    }

    fn sendPrompt(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const text = blk: {
            const p = params orelse return error.InvalidParams;
            switch (p) {
                .object => |obj| {
                    const text_val = obj.get("text") orelse return error.InvalidParams;
                    switch (text_val) {
                        .string => |s| break :blk s,
                        else => return error.InvalidParams,
                    }
                },
                else => return error.InvalidParams,
            }
        };

        if (self.agent_instance) |a| {
            try a.prompt(text);
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"ok\":true}}", .{});
        return buf.written();
    }

    fn setModel(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const model = blk: {
            const p = params orelse return error.InvalidParams;
            switch (p) {
                .object => |obj| {
                    const val = obj.get("model") orelse return error.InvalidParams;
                    switch (val) {
                        .string => |s| break :blk s,
                        else => return error.InvalidParams,
                    }
                },
                else => return error.InvalidParams,
            }
        };

        if (self.agent_instance) |a| {
            a.setModel(model);
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"ok\":true}}", .{});
        return buf.written();
    }

    fn getTools(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("[", .{});
        if (self.tool_registry) |tr| {
            var it = tr.tools.iterator();
            var first = true;
            while (it.next()) |entry| {
                if (!first) try out.print(",", .{});
                first = false;
                try out.print("{{\"name\":\"{s}\",\"description\":\"{s}\"}}", .{
                    entry.key_ptr.*,
                    entry.value_ptr.description,
                });
            }
        }
        try out.print("]", .{});
        return buf.written();
    }

    fn getPlugins(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("[", .{});
        if (self.plugin_registry) |pr| {
            for (pr.plugins.items, 0..) |p, i| {
                if (i > 0) try out.print(",", .{});
                try out.print("{{\"id\":\"{s}\",\"name\":\"{s}\",\"ready\":{},\"tools\":{d},\"commands\":{d}}}", .{
                    p.id, p.name, p.ready, p.registered_tools.items.len, p.registered_commands.items.len,
                });
            }
        }
        try out.print("]", .{});
        return buf.written();
    }

    fn getMessages(self: *Server, arena: std.mem.Allocator) ![]const u8 {
        var buf: std.Io.Writer.Allocating = .init(arena);
        const out = &buf.writer;
        try out.print("[", .{});
        if (self.agent_instance) |a| {
            for (a.messages.items, 0..) |msg, i| {
                if (i > 0) try out.print(",", .{});
                const role_str = switch (msg.role) {
                    .user => "user",
                    .assistant => "assistant",
                    .system => "system",
                    .tool => "tool",
                };
                try out.print("{{\"role\":\"{s}\",\"content\":", .{role_str});
                try std.json.Stringify.value(msg.content, .{}, out);
                try out.print("}}", .{});
            }
        }
        try out.print("]", .{});
        return buf.written();
    }

    fn callTool(self: *Server, arena: std.mem.Allocator, params: ?std.json.Value) ![]const u8 {
        const p = params orelse return error.InvalidParams;
        const obj = switch (p) {
            .object => |o| o,
            else => return error.InvalidParams,
        };
        const name = switch (obj.get("name") orelse return error.InvalidParams) {
            .string => |s| s,
            else => return error.InvalidParams,
        };
        const arguments = blk: {
            const args_val = obj.get("arguments") orelse break :blk "{}";
            switch (args_val) {
                .string => |s| break :blk s,
                else => break :blk "{}",
            }
        };

        if (self.tool_registry) |tr| {
            if (tr.get(name)) |tool| {
                const result = try tool.execute(tool.ctx, self.allocator, arguments);
                defer self.allocator.free(result.content);

                var buf: std.Io.Writer.Allocating = .init(arena);
                const out = &buf.writer;
                try out.print("{{\"content\":", .{});
                try std.json.Stringify.value(result.content, .{}, out);
                try out.print(",\"is_error\":{}}}", .{result.is_error});
                return buf.written();
            }
        }

        var buf: std.Io.Writer.Allocating = .init(arena);
        try buf.writer.print("{{\"error\":\"tool not found\"}}", .{});
        return buf.written();
    }

    fn writeResult(out: *std.Io.Writer, id: ?std.json.Value, result: []const u8) !void {
        try out.print("{{\"jsonrpc\":\"2.0\",\"id\":", .{});
        if (id) |v| {
            try std.json.Stringify.value(v, .{}, out);
        } else {
            try out.print("null", .{});
        }
        try out.print(",\"result\":{s}}}", .{result});
    }

    fn writeError(out: *std.Io.Writer, id: ?std.json.Value, code: i32, message: []const u8) !void {
        try out.print("{{\"jsonrpc\":\"2.0\",\"id\":", .{});
        if (id) |v| {
            try std.json.Stringify.value(v, .{}, out);
        } else {
            try out.print("null", .{});
        }
        try out.print(",\"error\":{{\"code\":{d},\"message\":\"{s}\"}}}}", .{ code, message });
    }
};

fn pushEvent(w: *std.Io.Writer, event: agent.Event) !void {
    var buf: std.Io.Writer.Allocating = .init(std.heap.page_allocator);
    defer buf.deinit();
    const out = &buf.writer;

    try out.print("{{\"jsonrpc\":\"2.0\",\"method\":\"event\",\"params\":{{\"type\":\"{s}\"", .{eventTypeName(event)});

    switch (event) {
        .message_start => |msg| {
            try out.print(",\"role\":\"{s}\"", .{roleName(msg.role)});
        },
        .message_update => |text| {
            try out.print(",\"delta\":", .{});
            try std.json.Stringify.value(text, .{}, out);
        },
        .message_end => |msg| {
            try out.print(",\"role\":\"{s}\",\"content\":", .{roleName(msg.role)});
            try std.json.Stringify.value(msg.content, .{}, out);
        },
        .tool_call => |tc| {
            try out.print(",\"tool_name\":\"{s}\",\"arguments\":", .{tc.name});
            try std.json.Stringify.value(tc.arguments, .{}, out);
        },
        .tool_execution_start => |tc| {
            try out.print(",\"tool_name\":\"{s}\"", .{tc.name});
        },
        .tool_execution_update => |text| {
            try out.print(",\"delta\":", .{});
            try std.json.Stringify.value(text, .{}, out);
        },
        .tool_execution_end => |tr| {
            try out.print(",\"is_error\":{}", .{tr.is_error});
        },
        .tool_result => |tr| {
            try out.print(",\"is_error\":{}", .{tr.is_error});
        },
        .before_agent_start => |bas| {
            try out.print(",\"message_count\":{d}", .{bas.messages.len});
        },
        else => {},
    }

    try out.print("}}}}", .{});
    try w.print("{s}\n", .{buf.written()});
    try w.flush();
}

fn eventTypeName(event: agent.Event) []const u8 {
    return switch (event) {
        .before_agent_start => "before_agent_start",
        .agent_start => "agent_start",
        .turn_start => "turn_start",
        .message_start => "message_start",
        .message_update => "message_update",
        .message_end => "message_end",
        .tool_call => "tool_call",
        .tool_execution_start => "tool_execution_start",
        .tool_execution_update => "tool_execution_update",
        .tool_execution_end => "tool_execution_end",
        .tool_result => "tool_result",
        .turn_end => "turn_end",
        .agent_end => "agent_end",
    };
}

fn roleName(role: provider.Role) []const u8 {
    return switch (role) {
        .user => "user",
        .assistant => "assistant",
        .system => "system",
        .tool => "tool",
    };
}

const Request = struct {
    jsonrpc: []const u8 = "2.0",
    id: ?std.json.Value = null,
    method: []const u8,
    params: ?std.json.Value = null,
};

test "ipc server init/deinit" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var server = Server.init(gpa, io, "/tmp/telekinesis-test.sock");
    defer server.deinit();
    try std.testing.expectEqualStrings("/tmp/telekinesis-test.sock", server.socket_path);
}

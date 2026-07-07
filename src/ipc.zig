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

    pub fn init(allocator: std.mem.Allocator, io: std.Io, socket_path: []const u8) Server {
        return .{
            .allocator = allocator,
            .io = io,
            .socket_path = socket_path,
            .arena = std.heap.ArenaAllocator.init(allocator),
        };
    }

    pub fn deinit(self: *Server) void {
        self.arena.deinit();
    }

    pub fn attachAgent(self: *Server, a: *agent.Agent) void {
        self.agent_instance = a;
    }

    pub fn attachTools(self: *Server, tr: *agent.ToolRegistry) void {
        self.tool_registry = tr;
    }

    pub fn attachPlugins(self: *Server, pr: *plugin.Registry) void {
        self.plugin_registry = pr;
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

            const response = try self.handleRequest(arena, line.?);
            try w.print("{s}\n", .{response});
            try w.flush();
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
        if (std.mem.eql(u8, method, "state")) {
            return self.getState(arena);
        }
        if (std.mem.eql(u8, method, "prompt")) {
            return self.sendPrompt(arena, params);
        }
        if (std.mem.eql(u8, method, "set_model")) {
            return self.setModel(arena, params);
        }
        if (std.mem.eql(u8, method, "tools")) {
            return self.getTools(arena);
        }
        if (std.mem.eql(u8, method, "plugins")) {
            return self.getPlugins(arena);
        }
        if (std.mem.eql(u8, method, "messages")) {
            return self.getMessages(arena);
        }
        return error.MethodNotFound;
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

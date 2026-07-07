const std = @import("std");

const log = std.log.scoped(.lsp);

pub const LanguageId = []const u8;

pub const Diagnostic = struct {
    uri: []const u8,
    line: u32,
    character: u32,
    severity: Severity,
    message: []const u8,
    source: ?[]const u8 = null,

    pub const Severity = enum(u8) {
        err = 1,
        warn = 2,
        info = 3,
        hint = 4,
    };
};

pub const Location = struct {
    uri: []const u8,
    start_line: u32,
    start_character: u32,
    end_line: u32,
    end_character: u32,
};

pub const Hover = struct {
    contents: []const u8,
};

pub const ServerConfig = struct {
    language: []const u8,
    command: []const u8,
    args: []const []const u8 = &.{},
};

pub const default_servers = [_]ServerConfig{
    .{ .language = "zig", .command = "zls" },
    .{ .language = "rust", .command = "rust-analyzer" },
    .{ .language = "typescript", .command = "typescript-language-server", .args = &.{"--stdio"} },
    .{ .language = "go", .command = "gopls" },
    .{ .language = "python", .command = "pylsp" },
    .{ .language = "c", .command = "clangd" },
    .{ .language = "cpp", .command = "clangd" },
    .{ .language = "javascript", .command = "typescript-language-server", .args = &.{"--stdio"} },
};

pub const Client = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    language: []const u8,
    command: []const u8,
    args: []const []const u8,
    process: ?std.process.Child = null,
    initialized: bool = false,
    next_request_id: u64 = 1,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, config: ServerConfig) Client {
        return .{
            .allocator = allocator,
            .io = io,
            .language = try allocator.dupe(u8, config.language),
            .command = try allocator.dupe(u8, config.command),
            .args = config.args,
        };
    }

    pub fn deinit(self: *Client) void {
        self.stop();
        self.allocator.free(self.language);
        self.allocator.free(self.command);
    }

    pub fn start(self: *Client) !void {
        var child = std.process.Child.init(self.io);
        var argv: std.ArrayList([]const u8) = .empty;
        defer argv.deinit(self.allocator);
        try argv.append(self.allocator, self.command);
        for (self.args) |arg| {
            try argv.append(self.allocator, arg);
        }
        child.argv = argv.items;
        child.stdin_behavior = .Pipe;
        child.stdout_behavior = .Pipe;
        child.stderr_behavior = .Inherit;
        try child.spawn();
        self.process = child;
        log.info("started LSP for {s}: {s}", .{ self.language, self.command });
    }

    pub fn stop(self: *Client) void {
        if (self.process) |*p| {
            p.kill(self.io);
            _ = p.wait(self.io) catch {};
            self.process = null;
        }
        self.initialized = false;
    }

    pub fn nextId(self: *Client) u64 {
        const id = self.next_request_id;
        self.next_request_id += 1;
        return id;
    }

    pub fn sendRequest(self: *Client, method: []const u8, params_json: []const u8) !u64 {
        if (self.process == null) return error.LspNotRunning;
        const file = self.process.?.stdin orelse return error.LspNotRunning;

        const id = self.nextId();

        var buf: [4096]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(file, .global, &buf);
        const writer = &file_writer.interface;

        var json_buf: std.Io.Writer.Allocating = .init(self.allocator);
        const jw = &json_buf.writer;
        defer json_buf.deinit();

        try jw.writeAll("{\"jsonrpc\":\"2.0\",\"id\":");
        try jw.print("{d}", .{id});
        try jw.writeAll(",\"method\":\"");
        try jw.writeAll(method);
        try jw.writeAll("\",\"params\":");
        try jw.writeAll(params_json);
        try jw.writeAll("}");

        const content = json_buf.written();
        try writer.print("Content-Length: {d}\r\n\r\n", .{content.len});
        try writer.writeAll(content);
        try writer.flush();

        return id;
    }

    pub fn sendNotification(self: *Client, method: []const u8, params_json: []const u8) !void {
        if (self.process == null) return error.LspNotRunning;
        const file = self.process.?.stdin orelse return error.LspNotRunning;

        var buf: [4096]u8 = undefined;
        var file_writer: std.Io.File.Writer = .init(file, .global, &buf);
        const writer = &file_writer.interface;

        var json_buf: std.Io.Writer.Allocating = .init(self.allocator);
        const jw = &json_buf.writer;
        defer json_buf.deinit();

        try jw.writeAll("{\"jsonrpc\":\"2.0\",\"method\":\"");
        try jw.writeAll(method);
        try jw.writeAll("\",\"params\":");
        try jw.writeAll(params_json);
        try jw.writeAll("}");

        const content = json_buf.written();
        try writer.print("Content-Length: {d}\r\n\r\n", .{content.len});
        try writer.writeAll(content);
        try writer.flush();
    }

    pub fn initialize(self: *Client, root_uri: []const u8) !void {
        var params_buf: std.Io.Writer.Allocating = .init(self.allocator);
        const pw = &params_buf.writer;
        defer params_buf.deinit();

        try pw.writeAll("{\"processId\":null,\"rootUri\":\"");
        try pw.writeAll(root_uri);
        try pw.writeAll("\",\"capabilities\":{}}");

        _ = try self.sendRequest("initialize", params_buf.written());
        try self.sendNotification("initialized", "{}");
        self.initialized = true;
        log.info("LSP {s} initialized", .{self.language});
    }
};

pub const Manager = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    clients: std.StringHashMap(*Client),
    configs: std.StringHashMap(ServerConfig),

    pub fn init(allocator: std.mem.Allocator, io: std.Io) Manager {
        return .{
            .allocator = allocator,
            .io = io,
            .clients = std.StringHashMap(*Client).init(allocator),
            .configs = std.StringHashMap(ServerConfig).init(allocator),
        };
    }

    pub fn deinit(self: *Manager) void {
        var iter = self.clients.iterator();
        while (iter.next()) |entry| {
            entry.value_ptr.*.deinit();
            self.allocator.destroy(entry.value_ptr.*);
        }
        self.clients.deinit();
        self.configs.deinit();
    }

    pub fn registerServer(self: *Manager, config: ServerConfig) !void {
        try self.configs.put(config.language, config);
        log.info("registered LSP server for {s}: {s}", .{ config.language, config.command });
    }

    pub fn startForLanguage(self: *Manager, language: []const u8) !*Client {
        const config = self.configs.get(language) orelse blk: {
            for (default_servers) |dc| {
                if (std.mem.eql(u8, dc.language, language)) break :blk dc;
            }
            return error.NoServerForLanguage;
        };

        const client = try self.allocator.create(Client);
        client.* = Client.init(self.allocator, self.io, config);
        try client.start();
        try self.clients.put(language, client);
        return client;
    }

    pub fn get(self: *Manager, language: []const u8) ?*Client {
        return self.clients.get(language);
    }

    pub fn stopAll(self: *Manager) void {
        var iter = self.clients.iterator();
        while (iter.next()) |entry| {
            entry.value_ptr.*.stop();
        }
    }

    pub fn supportedLanguages(self: *const Manager, allocator: std.mem.Allocator) ![][]const u8 {
        var result: std.ArrayList([]const u8) = .empty;
        var iter = self.configs.iterator();
        while (iter.next()) |entry| {
            try result.append(allocator, entry.key_ptr.*);
        }
        for (default_servers) |dc| {
            if (!self.configs.contains(dc.language)) {
                try result.append(allocator, dc.language);
            }
        }
        return result.toOwnedSlice(allocator);
    }
};

test "manager registers and starts server config" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var manager = Manager.init(gpa, io);
    defer manager.deinit();

    try manager.registerServer(.{
        .language = "custom",
        .command = "custom-lsp",
    });

    try std.testing.expect(manager.configs.contains("custom"));
}

test "manager finds default server" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var manager = Manager.init(gpa, io);
    defer manager.deinit();

    const langs = try manager.supportedLanguages(gpa);
    defer gpa.free(langs);

    var found_zig = false;
    for (langs) |lang| {
        if (std.mem.eql(u8, lang, "zig")) found_zig = true;
    }
    try std.testing.expect(found_zig);
}

test "client next id increments" {
    const gpa = std.testing.allocator;
    const io = std.Io.Threaded.global_single_threaded.ioBasic();
    var client = Client.init(gpa, io, .{ .language = "zig", .command = "zls" });
    defer client.deinit();

    try std.testing.expectEqual(@as(u64, 1), client.nextId());
    try std.testing.expectEqual(@as(u64, 2), client.nextId());
    try std.testing.expectEqual(@as(u64, 3), client.nextId());
}

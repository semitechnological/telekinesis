const std = @import("std");
const agent = @import("agent.zig");

const log = std.log.scoped(.tools);

var global_io: ?std.Io = null;

pub fn setIo(io: std.Io) void {
    global_io = io;
}

fn io_global() std.Io {
    return global_io orelse std.Io.Threaded.global_single_threaded.io();
}

pub fn registerBuiltins(registry: *agent.ToolRegistry) !void {
    try registry.register(.{
        .name = "read_file",
        .description = "Read the contents of a file at the given path.",
        .parameters_json =
            \\{"type":"object","properties":{"path":{"type":"string","description":"Absolute or relative file path"}},"required":["path"]}
        ,
        .execute = readFileExecute,
    });

    try registry.register(.{
        .name = "write_file",
        .description = "Write content to a file, creating or overwriting it.",
        .parameters_json =
            \\{"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"]}
        ,
        .execute = writeFileExecute,
    });

    try registry.register(.{
        .name = "list_dir",
        .description = "List entries in a directory.",
        .parameters_json =
            \\{"type":"object","properties":{"path":{"type":"string","description":"Directory path"}},"required":["path"]}
        ,
        .execute = listDirExecute,
    });

    try registry.register(.{
        .name = "run_command",
        .description = "Run a shell command and return stdout/stderr.",
        .parameters_json =
            \\{"type":"object","properties":{"command":{"type":"string"},"cwd":{"type":"string"}},"required":["command"]}
        ,
        .execute = runCommandExecute,
    });
}

fn parsePathArg(allocator: std.mem.Allocator, arguments: []const u8) ![]const u8 {
    const parsed = try std.json.parseFromSlice(PathArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();
    return try allocator.dupe(u8, parsed.value.path);
}

const PathArgs = struct {
    path: []const u8,
};

fn readFileExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const path = try parsePathArg(allocator, arguments);
    defer allocator.free(path);

    const io = io_global();
    const file = std.Io.Dir.cwd().openFile(io, path, .{}) catch |err| {
        return .{
            .id = "read_file",
            .content = try std.fmt.allocPrint(allocator, "Failed to open {s}: {}", .{ path, err }),
            .is_error = true,
        };
    };
    defer file.close(io);

    var read_buf: [4096]u8 = undefined;
    var content_buf: std.Io.Writer.Allocating = .init(allocator);
    const out = &content_buf.writer;
    defer content_buf.deinit();

    var file_reader = file.readerStreaming(io, &read_buf);
    const reader = &file_reader.interface;
    while (true) {
        const n = reader.readSliceShort(&read_buf) catch break;
        if (n == 0) break;
        try out.writeAll(read_buf[0..n]);
    }

    return .{
        .id = "read_file",
        .content = try allocator.dupe(u8, content_buf.written()),
        .is_error = false,
    };
}

fn writeFileExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const parsed = try std.json.parseFromSlice(WriteFileArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    const path = parsed.value.path;
    const content = parsed.value.content;
    const io = io_global();

    const file = std.Io.Dir.cwd().createFile(io, path, .{ .truncate = true }) catch |err| {
        return .{
            .id = "write_file",
            .content = try std.fmt.allocPrint(allocator, "Failed to create {s}: {}", .{ path, err }),
            .is_error = true,
        };
    };
    defer file.close(io);

    file.writeStreamingAll(io, content) catch |err| {
        return .{
            .id = "write_file",
            .content = try std.fmt.allocPrint(allocator, "Failed to write {s}: {}", .{ path, err }),
            .is_error = true,
        };
    };

    return .{
        .id = "write_file",
        .content = try std.fmt.allocPrint(allocator, "Wrote {d} bytes to {s}", .{ content.len, path }),
        .is_error = false,
    };
}

const WriteFileArgs = struct {
    path: []const u8,
    content: []const u8,
};

fn listDirExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const path = try parsePathArg(allocator, arguments);
    defer allocator.free(path);

    const io = io_global();
    var dir = std.Io.Dir.cwd().openDir(io, path, .{ .iterate = true }) catch |err| {
        return .{
            .id = "list_dir",
            .content = try std.fmt.allocPrint(allocator, "Failed to open dir {s}: {}", .{ path, err }),
            .is_error = true,
        };
    };
    defer dir.close(io);

    var buf: std.Io.Writer.Allocating = .init(allocator);
    const out = &buf.writer;

    var iter = dir.iterate();
    while (try iter.next(io)) |entry| {
        const type_str = switch (entry.kind) {
            .file => "F",
            .directory => "D",
            .sym_link => "L",
            else => "?",
        };
        try out.print("{s} {s}\n", .{ type_str, entry.name });
    }

    return .{
        .id = "list_dir",
        .content = try allocator.dupe(u8, buf.written()),
        .is_error = false,
    };
}

fn runCommandExecute(ctx: ?*anyopaque, allocator: std.mem.Allocator, arguments: []const u8) !agent.ToolResult {
    _ = ctx;
    const parsed = try std.json.parseFromSlice(RunCommandArgs, allocator, arguments, .{
        .ignore_unknown_fields = true,
    });
    defer parsed.deinit();

    const command = parsed.value.command;
    const cwd = parsed.value.cwd;
    const io = io_global();

    const cwd_opt: std.process.Child.Cwd = if (cwd) |c| .{ .path = c } else .inherit;

    const argv: []const []const u8 = switch (@import("builtin").os.tag) {
        .windows => &.{ "cmd.exe", "/c", command },
        else => &.{ "sh", "-c", command },
    };

    var child = std.process.spawn(io, .{
        .argv = argv,
        .cwd = cwd_opt,
        .stdout = .pipe,
        .stderr = .pipe,
    }) catch |err| {
        return .{
            .id = "run_command",
            .content = try std.fmt.allocPrint(allocator, "Failed to spawn: {}", .{err}),
            .is_error = true,
        };
    };

    var stdout_buf: std.Io.Writer.Allocating = .init(allocator);
    const out = &stdout_buf.writer;
    defer stdout_buf.deinit();

    if (child.stdout) |stdout_file| {
        var read_buf: [4096]u8 = undefined;
        var file_reader = stdout_file.readerStreaming(io, &read_buf);
        const reader = &file_reader.interface;
        while (true) {
            const n = reader.readSliceShort(&read_buf) catch break;
            if (n == 0) break;
            try out.writeAll(read_buf[0..n]);
        }
    }

    const term = child.wait(io) catch |err| {
        return .{
            .id = "run_command",
            .content = try std.fmt.allocPrint(allocator, "Wait failed: {}", .{err}),
            .is_error = true,
        };
    };

    const exit_code: i32 = switch (term) {
        .exited => |code| @intCast(code),
        else => -1,
    };

    var result_buf: std.Io.Writer.Allocating = .init(allocator);
    const result_out = &result_buf.writer;
    try result_out.print("exit: {d}\n{s}", .{ exit_code, stdout_buf.written() });

    return .{
        .id = "run_command",
        .content = try allocator.dupe(u8, result_buf.written()),
        .is_error = exit_code != 0,
    };
}

const RunCommandArgs = struct {
    command: []const u8,
    cwd: ?[]const u8 = null,
};
